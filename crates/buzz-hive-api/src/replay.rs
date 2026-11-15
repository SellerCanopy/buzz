//! In-memory seen-set for NIP-98 event ids. One `buzz-hive` process serves a community.
//!
//! Ids are kept in arrival order beside a lookup set, so dropping the expired ones pops from
//! the front and costs only what it removes. The set has a ceiling: when it is full the
//! guard refuses new ids instead of growing, so a flood cannot take the service's memory.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an id is remembered: twice the 60 second verification tolerance,
/// so an id cannot outlive its validity window.
pub const REPLAY_TTL: Duration = Duration::from_secs(120);

/// Most ids held at once. A member signs in once per token, so this is far above real use.
pub const REPLAY_CAPACITY: usize = 100_000;

/// What the guard decided about an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// First use. The id is now remembered.
    New,
    /// The id was used already within its lifetime.
    Replayed,
    /// The guard is full, so the id was not remembered. Refuse the request.
    Full,
}

pub struct ReplayGuard {
    inner: Mutex<Inner>,
    ttl: Duration,
    capacity: usize,
}

#[derive(Default)]
struct Inner {
    /// Oldest first, which is also earliest to expire.
    order: VecDeque<(Instant, String)>,
    ids: HashSet<String>,
}

impl ReplayGuard {
    pub fn new(ttl: Duration) -> Self {
        Self::with_capacity(ttl, REPLAY_CAPACITY)
    }

    pub fn with_capacity(ttl: Duration, capacity: usize) -> Self {
        Self {
            inner: Mutex::default(),
            ttl,
            capacity,
        }
    }

    /// Remember `id` if it is new. Drops the ids that have expired by `now` first.
    pub fn first_use(&self, id: &str, now: Instant) -> Seen {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        while let Some((at, _)) = inner.order.front() {
            if now.saturating_duration_since(*at) < self.ttl {
                break;
            }
            if let Some((_, expired)) = inner.order.pop_front() {
                inner.ids.remove(&expired);
            }
        }
        if inner.ids.contains(id) {
            return Seen::Replayed;
        }
        if inner.ids.len() >= self.capacity {
            return Seen::Full;
        }
        inner.ids.insert(id.to_owned());
        inner.order.push_back((now, id.to_owned()));
        Seen::New
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_use_is_refused() {
        let guard = ReplayGuard::new(REPLAY_TTL);
        let now = Instant::now();
        assert_eq!(guard.first_use("a", now), Seen::New);
        assert_eq!(
            guard.first_use("a", now + Duration::from_secs(1)),
            Seen::Replayed
        );
        assert_eq!(guard.first_use("b", now), Seen::New);
    }

    #[test]
    fn an_id_is_accepted_again_after_the_ttl() {
        let guard = ReplayGuard::new(REPLAY_TTL);
        let now = Instant::now();
        assert_eq!(guard.first_use("a", now), Seen::New);
        assert_eq!(
            guard.first_use("a", now + Duration::from_secs(119)),
            Seen::Replayed
        );
        assert_eq!(
            guard.first_use("a", now + Duration::from_secs(121)),
            Seen::New
        );
    }

    #[test]
    fn a_refused_replay_does_not_extend_the_id() {
        let guard = ReplayGuard::new(REPLAY_TTL);
        let now = Instant::now();
        assert_eq!(guard.first_use("a", now), Seen::New);
        assert_eq!(
            guard.first_use("a", now + Duration::from_secs(100)),
            Seen::Replayed
        );
        // The id still expires 120 seconds after its first use, not after the replay.
        assert_eq!(
            guard.first_use("a", now + Duration::from_secs(121)),
            Seen::New
        );
    }

    #[test]
    fn a_full_guard_refuses_new_ids_until_old_ones_expire() {
        let guard = ReplayGuard::with_capacity(REPLAY_TTL, 3);
        let now = Instant::now();
        for id in ["a", "b", "c"] {
            assert_eq!(guard.first_use(id, now), Seen::New);
        }
        assert_eq!(guard.first_use("d", now), Seen::Full);
        // A known id is still a replay, not a full guard.
        assert_eq!(guard.first_use("a", now), Seen::Replayed);
        let later = now + REPLAY_TTL + Duration::from_secs(1);
        assert_eq!(guard.first_use("d", later), Seen::New);
    }

    #[test]
    fn expiry_removes_only_what_is_old() {
        let guard = ReplayGuard::with_capacity(REPLAY_TTL, 10);
        let start = Instant::now();
        assert_eq!(guard.first_use("old", start), Seen::New);
        assert_eq!(
            guard.first_use("young", start + Duration::from_secs(100)),
            Seen::New
        );
        let at = start + Duration::from_secs(130);
        assert_eq!(guard.first_use("old", at), Seen::New);
        assert_eq!(guard.first_use("young", at), Seen::Replayed);
    }
}
