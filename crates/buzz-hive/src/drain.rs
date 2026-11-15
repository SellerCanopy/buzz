//! The loop that sends queued posts to the relay.
//!
//! A start wakes it at once. A long timer is the backstop for a wake that was lost. A card
//! whose post failed waits longer each time, up to a cap, so a relay that is down or has
//! just restarted is not hit by every instance at the same moment. The wait belongs to the
//! card: other cards' posts keep going out, and a wake still starts a pass.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use buzz_hive_core::bus::Bus;
use buzz_hive_core::orchestrator::outbox::drain_outbox;
use buzz_hive_core::orchestrator::Env;
use buzz_hive_core::ports::relay::Relay;
use buzz_hive_core::ports::store::{CommunityId, Directory, Store};
use buzz_hive_core::workflow::load::Catalog;
use tokio::sync::{watch, Notify};
use uuid::Uuid;

/// How long to wait with nothing to send and nothing failed.
const BACKSTOP: Duration = Duration::from_secs(5 * 60);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const MAX_RETRY: Duration = Duration::from_secs(5 * 60);
/// The first pass starts within this long, so a restarted fleet does not send as one.
const START_JITTER_MS: u64 = 2_000;

pub struct Drain {
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn Directory>,
    pub relay: Arc<dyn Relay>,
    pub bus: Bus,
    pub community: CommunityId,
    pub hive: String,
    /// Woken by a commit that queued a post.
    pub wake: Arc<Notify>,
}

/// A card whose last post failed: how many times in a row, and when it may be tried again.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    failures: u32,
    retry_at: Instant,
}

/// What the loop remembers between passes.
#[derive(Default)]
struct State {
    /// Cards waiting out a failed post. A card in here is skipped until its time comes, so
    /// one bad post delays only its own card.
    cards: HashMap<Uuid, Backoff>,
    /// Passes in a row that could not read the outbox at all.
    unreadable: u32,
}

impl State {
    /// The cards still waiting out their backoff at `now`.
    fn waiting(&self, now: Instant) -> Vec<Uuid> {
        self.cards
            .iter()
            .filter(|(_, backoff)| backoff.retry_at > now)
            .map(|(card, _)| *card)
            .collect()
    }

    /// Takes a pass's outcome in. A card that was tried and did not fail again is clear.
    fn learn(&mut self, now: Instant, skipped: &[Uuid], failed: &[Uuid]) {
        self.cards
            .retain(|card, _| skipped.contains(card) || failed.contains(card));
        for card in failed {
            let failures = self
                .cards
                .get(card)
                .map_or(1, |backoff| backoff.failures.saturating_add(1));
            self.cards.insert(
                *card,
                Backoff {
                    failures,
                    retry_at: now + retry_after(failures),
                },
            );
        }
    }

    /// How long to sleep: until the soonest card may be tried again, or the backstop.
    fn sleep(&self, now: Instant) -> Duration {
        self.cards
            .values()
            .map(|backoff| backoff.retry_at.saturating_duration_since(now))
            .min()
            .map_or(BACKSTOP, |soonest| soonest.min(BACKSTOP))
    }
}

impl Drain {
    /// Runs until `stop` changes.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        let start = Duration::from_millis(rand::random_range(0..START_JITTER_MS));
        if sleep_or_stop(start, &mut stop).await {
            return;
        }
        let mut state = State::default();
        loop {
            self.pass(&mut state, &stop).await;
            let wait = if state.unreadable > 0 {
                // The store is down, so a wake would only fail again: back off.
                retry_after(state.unreadable)
            } else {
                state.sleep(Instant::now())
            };
            let stopped = if state.unreadable > 0 {
                sleep_or_stop(wait, &mut stop).await
            } else {
                // A wake starts a pass at once. Cards still in backoff are skipped by it, so a
                // new post for another card goes out without waiting for them.
                tokio::select! {
                    _ = self.wake.notified() => false,
                    stopped = sleep_or_stop(wait, &mut stop) => stopped,
                }
            };
            if stopped || *stop.borrow() {
                return;
            }
        }
    }

    /// One pass over the outbox.
    async fn pass(&self, state: &mut State, stop: &watch::Receiver<bool>) {
        let catalog = Catalog::default();
        let env = Env {
            store: self.store.as_ref(),
            directory: self.directory.as_ref(),
            catalog: &catalog,
            hive: &self.hive,
        };
        let now = Instant::now();
        let skipped = state.waiting(now);
        match drain_outbox(&env, self.community, self.relay.as_ref(), &skipped, stop).await {
            Ok(done) => {
                state.unreadable = 0;
                self.bus.publish(&done.events);
                if done.posted > 0 {
                    tracing::info!(posted = done.posted, "posts sent to the relay");
                }
                if done.failed > 0 {
                    tracing::warn!(
                        failed = done.failed,
                        cards = done.failed_cards.len(),
                        error = done.first_error.as_deref().unwrap_or_default(),
                        "some posts were not sent; they stay queued"
                    );
                }
                state.learn(now, &skipped, &done.failed_cards);
            }
            Err(error) => {
                tracing::warn!(%error, "cannot read the outbox");
                state.unreadable = state.unreadable.saturating_add(1);
            }
        }
    }
}

/// How long to wait after `failures` failures in a row: 2 s, doubling, capped at five
/// minutes, with up to a quarter added so instances drift apart.
pub fn retry_after(failures: u32) -> Duration {
    let base = FIRST_RETRY
        .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
        .min(MAX_RETRY);
    let extra = rand::random_range(0..=base.as_millis() as u64 / 4);
    base + Duration::from_millis(extra)
}

/// Sleeps for `wait`. Returns true when `stop` changed first.
async fn sleep_or_stop(wait: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(wait) => false,
        _ = stop.changed() => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_card_waits_while_others_are_not_held_back() {
        let (bad, good) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut state = State::default();
        let now = Instant::now();
        state.learn(now, &[], &[bad]);
        assert_eq!(state.waiting(now), vec![bad]);
        // The sleep is the card's wait, not the backstop.
        assert!(state.sleep(now) <= FIRST_RETRY * 5 / 4);
        // Another card never enters the wait list.
        assert!(!state.waiting(now).contains(&good));
        // Once its time has come the card is tried again, and a clean pass clears it.
        let later = now + MAX_RETRY * 2;
        assert!(state.waiting(later).is_empty());
        state.learn(later, &[], &[]);
        assert!(state.cards.is_empty());
        assert_eq!(state.sleep(later), BACKSTOP);
    }

    #[test]
    fn repeated_failures_of_one_card_lengthen_its_wait() {
        let card = Uuid::from_u128(1);
        let mut state = State::default();
        let mut now = Instant::now();
        for _ in 0..4 {
            state.learn(now, &[], &[card]);
            now += MAX_RETRY * 2;
        }
        assert_eq!(state.cards[&card].failures, 4);
    }

    #[test]
    fn a_card_still_waiting_keeps_its_count_through_a_pass_that_skipped_it() {
        let card = Uuid::from_u128(1);
        let mut state = State::default();
        let now = Instant::now();
        state.learn(now, &[], &[card]);
        state.learn(now, &[card], &[]);
        assert_eq!(state.cards[&card].failures, 1);
    }

    #[test]
    fn the_wait_doubles_and_stops_at_the_cap() {
        for _ in 0..50 {
            let first = retry_after(1);
            assert!(
                first >= FIRST_RETRY && first <= FIRST_RETRY * 5 / 4,
                "{first:?}"
            );
            let third = retry_after(3);
            assert!(third >= Duration::from_secs(8), "{third:?}");
            let late = retry_after(40);
            assert!(late >= MAX_RETRY && late <= MAX_RETRY * 5 / 4, "{late:?}");
        }
    }
}
