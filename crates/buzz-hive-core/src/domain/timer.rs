//! A timer a run waits on: the stage's timeout, or the wait before a retry.

use uuid::Uuid;

/// What a timer does when it fires. The stored text is the lowercase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    /// The stage's agent did not report in time.
    Timeout,
    /// The wait before another attempt is over.
    Retry,
}

impl TimerKind {
    /// The value stored in `hive.timers.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Retry => "retry",
        }
    }

    /// The inverse of [`TimerKind::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "timeout" => Some(Self::Timeout),
            "retry" => Some(Self::Retry),
            _ => None,
        }
    }
}

/// A timer that has come due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    pub run_id: Uuid,
    pub stage: String,
    pub kind: TimerKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_stored_name() {
        for kind in [TimerKind::Timeout, TimerKind::Retry] {
            assert_eq!(TimerKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(TimerKind::parse("later"), None);
    }
}
