//! A token bucket for the one route that answers anyone who can sign an event.

use std::sync::Mutex;
use std::time::Instant;

/// Sign-ins served a second, on average. A community signs in once per token, so this is far
/// above real use.
pub const AUTH_PER_SECOND: u32 = 50;

/// Sign-ins that may arrive at once, such as every client reconnecting after a restart.
pub const AUTH_BURST: u32 = 200;

/// The limiter `/auth/token` runs behind.
pub fn auth_limiter() -> std::sync::Arc<RateLimiter> {
    std::sync::Arc::new(RateLimiter::new(AUTH_PER_SECOND, AUTH_BURST))
}

/// Allows `per_second` calls a second on average, with bursts up to `burst`.
pub struct RateLimiter {
    state: Mutex<Bucket>,
    per_second: f64,
    burst: f64,
}

struct Bucket {
    tokens: f64,
    at: Option<Instant>,
}

impl RateLimiter {
    pub fn new(per_second: u32, burst: u32) -> Self {
        Self {
            state: Mutex::new(Bucket {
                tokens: f64::from(burst),
                at: None,
            }),
            per_second: f64::from(per_second),
            burst: f64::from(burst),
        }
    }

    /// Takes one call from the bucket. False when it is empty.
    pub fn try_acquire(&self, now: Instant) -> bool {
        let mut bucket = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = bucket.at {
            let refill = now.saturating_duration_since(at).as_secs_f64() * self.per_second;
            bucket.tokens = (bucket.tokens + refill).min(self.burst);
        }
        bucket.at = Some(now);
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_burst_passes_then_the_bucket_refills_with_time() {
        let limiter = RateLimiter::new(2, 3);
        let now = Instant::now();
        assert!((0..3).all(|_| limiter.try_acquire(now)));
        assert!(!limiter.try_acquire(now));
        // Half a second at two a second is one call.
        let later = now + Duration::from_millis(500);
        assert!(limiter.try_acquire(later));
        assert!(!limiter.try_acquire(later));
        // A long quiet spell refills no further than the burst.
        let much_later = later + Duration::from_secs(60);
        assert!((0..3).all(|_| limiter.try_acquire(much_later)));
        assert!(!limiter.try_acquire(much_later));
    }
}
