//! Agent activity: how many runs there have been, how long they took and how much the
//! community's agents started each week. A count of what happened, never of what it cost.

use chrono::{Datelike, Duration, NaiveDate};

/// How many weeks the Usage page shows, the current one last.
pub const USAGE_WEEKS: u32 = 8;

/// One week, starting Monday (UTC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWeek {
    pub starts: NaiveDate,
    /// Cards whose `card.created` event has an agent as its actor.
    pub cards_created_by_agents: u32,
    /// Runs whose `run.started` event has an agent as its actor.
    pub runs_started_by_agents: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// Every run the community has had.
    pub runs: u32,
    /// The runs that have ended, however they ended.
    pub ended_runs: u32,
    /// Mean time from start to end over the ended runs, in whole seconds. Zero with none.
    pub average_run_seconds: u64,
    /// The longest of those, in whole seconds.
    pub longest_run_seconds: u64,
    /// [`USAGE_WEEKS`] weeks, oldest first, with a row for a week that had nothing.
    pub weeks: Vec<UsageWeek>,
}

/// The Monday on or before `day`.
pub fn week_start(day: NaiveDate) -> NaiveDate {
    day - Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_week_starts_on_monday() {
        let monday = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        for offset in 0..7 {
            assert_eq!(week_start(monday + Duration::days(offset)), monday);
        }
        assert_eq!(
            week_start(monday + Duration::days(7)),
            monday + Duration::days(7)
        );
    }
}
