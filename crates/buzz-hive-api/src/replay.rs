//! In-memory seen-set for NIP-98 event ids. One `buzz-hive` process serves a community.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an id is remembered: twice the 60 second verification tolerance,
/// so an id cannot outlive its validity window.
pub const REPLAY_TTL: Duration = Duration::from_secs(120);

pub struct ReplayGuard {
    seen: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
}

impl ReplayGuard {
    pub fn new(ttl: Duration) -> Self {
        Self {
            seen: Mutex::default(),
            ttl,
        }
    }

    /// True when the id is new. Prunes expired ids on every call.
    pub fn first_use(&self, id: &str, now: Instant) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, at| now.duration_since(*at) < self.ttl);
        seen.insert(id.to_owned(), now).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_use_is_refused() {
        let guard = ReplayGuard::new(REPLAY_TTL);
        let now = Instant::now();
        assert!(guard.first_use("a", now));
        assert!(!guard.first_use("a", now + Duration::from_secs(1)));
        assert!(guard.first_use("b", now));
    }

    #[test]
    fn an_id_is_accepted_again_after_the_ttl() {
        let guard = ReplayGuard::new(REPLAY_TTL);
        let now = Instant::now();
        assert!(guard.first_use("a", now));
        assert!(!guard.first_use("a", now + Duration::from_secs(119)));
        assert!(guard.first_use("a", now + Duration::from_secs(121 + 119)));
    }
}
