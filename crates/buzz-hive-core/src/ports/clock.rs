//! The time a timer is measured against.
//!
//! Postgres holds the real timers and reads its own clock, so every instance and every
//! restart agrees on when a timer is due. This port is for the in-memory store, so a test
//! can move time by hand and see a timer fire at its boundary and not before.

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
