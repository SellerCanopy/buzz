use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::ports::clock::Clock;

/// A clock that moves only when a test moves it, so a timeout is tested at its boundary.
#[derive(Clone)]
pub struct ManualClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl ManualClock {
    /// A clock standing at a fixed, arbitrary time.
    pub fn new() -> Self {
        let start = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .expect("a valid time")
            .with_timezone(&Utc);
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: std::time::Duration) {
        let mut now = self.now.lock().unwrap();
        *now += chrono::Duration::from_std(by).expect("a duration that fits");
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }
}
