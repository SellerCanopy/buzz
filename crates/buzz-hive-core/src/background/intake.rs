//! A pass over an intake rule: read the repo's issues, make a card for each new match, and
//! mark the cards whose issue left the search.
//!
//! Host calls per pass: one list request per page of 100 open issues, so one for most
//! rules, and one read for each issue that left the search, up to [`MAX_DROPOUT_READS`].
//! Nothing else is read, and nothing is read from the relay.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::Instant;
use uuid::Uuid;

use crate::access::ActorKind;
use crate::domain::events::{Actor, Event};
use crate::domain::intake::{IntakeRule, Issue, IssueState, Page, LEFT_CLOSED, LEFT_UNMATCHED};
use crate::orchestrator::cards::{insert_card, NewCard};
use crate::orchestrator::intake::{card_description, card_title, IntakeError, PAGE_SIZE};
use crate::orchestrator::{Ctx, Env, OpError};
use crate::ports::code_host::{CodeHost, HostError};
use crate::ports::store::CommunityId;

/// Pages read per pass. A repo with more open issues than this is read in part.
pub const MAX_PAGES: u32 = 10;
/// Issues that left the search are read one by one to learn why, up to this many a pass.
pub const MAX_DROPOUT_READS: usize = 50;
/// The most a rule waits after failures, or a rate limit waits at all.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// A rule's pass starts, and repeats, up to this fraction of its interval late.
pub const JITTER: f64 = 0.2;

/// What a pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Open issues the rule takes.
    pub matched: usize,
    /// The cards the pass made.
    pub added: Vec<Uuid>,
    /// The cards whose issue left the search.
    pub left: usize,
    /// The repo had more open issues than a pass reads, so none was marked as left.
    pub truncated: bool,
}

/// How long after the last pass the next one starts. `unit` is a random number in `[0, 1)`,
/// passed in so a test chooses it.
pub fn next_wake(interval_s: u32, unit: f64) -> Duration {
    let interval = Duration::from_secs(u64::from(interval_s));
    interval + interval.mul_f64(JITTER * unit.clamp(0.0, 1.0))
}

/// How long a rule waits before its first pass: up to [`JITTER`] of its interval, so a fleet
/// that restarts together does not read together.
pub fn start_delay(interval_s: u32, unit: f64) -> Duration {
    Duration::from_secs(u64::from(interval_s)).mul_f64(JITTER * unit.clamp(0.0, 1.0))
}

/// How long to wait after `failures` failed passes in a row: the interval doubled for each,
/// up to [`MAX_BACKOFF`].
pub fn backoff(interval_s: u32, failures: u32) -> Duration {
    let mut wait = Duration::from_secs(u64::from(interval_s));
    for _ in 0..failures {
        if wait >= MAX_BACKOFF / 2 {
            return MAX_BACKOFF;
        }
        wait *= 2;
    }
    wait.min(MAX_BACKOFF)
}

/// How long to wait after the host asked for `retry_after`, on the `hits`th limit in a row:
/// at least what it asked, and longer each time, up to [`MAX_BACKOFF`].
pub fn rate_wait(retry_after: Duration, interval_s: u32, hits: u32) -> Duration {
    retry_after
        .max(backoff(interval_s, hits.saturating_sub(1)))
        .min(MAX_BACKOFF)
}

/// The time every rule holds off until after the host rate limited the token. One token is
/// shared by every rule, so one limit stops them all.
#[derive(Default)]
pub struct Gate {
    until: Mutex<Option<Instant>>,
}

impl Gate {
    /// Holds every caller of [`Gate::wait`] until `wait` from now, if that is later than it
    /// already holds them.
    pub async fn hold(&self, wait: Duration) {
        let until = Instant::now() + wait;
        let mut held = self.until.lock().await;
        if held.is_none_or(|have| have < until) {
            *held = Some(until);
        }
    }

    /// Returns when the gate is open. Time that passes while waiting may move the gate, so
    /// it looks again.
    pub async fn wait(&self) {
        loop {
            let until = *self.until.lock().await;
            match until {
                Some(until) if until > Instant::now() => tokio::time::sleep_until(until).await,
                _ => return,
            }
        }
    }
}

/// The actor of what the engine does by itself.
pub fn system(env: &Env<'_>) -> Actor {
    Actor {
        pubkey: env.hive.to_owned(),
        kind: ActorKind::Agent,
    }
}

/// Runs one pass over the rule, and returns what it did with the events it committed.
pub async fn refresh_rule(
    env: &Env<'_>,
    host: &dyn CodeHost,
    community: CommunityId,
    rule_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(Outcome, Vec<Event>), IntakeError> {
    let rule = {
        let mut tx = env.store.begin(community).await?;
        tx.intake()
            .get(rule_id)
            .await?
            .ok_or_else(|| OpError::NotFound(format!("intake rule {rule_id} does not exist")))?
    };
    let (open, truncated) = read_open(host, &rule).await?;
    let matches: Vec<Issue> = open
        .into_iter()
        .filter(|issue| rule.filter.takes(issue))
        .collect();

    // Cards whose issue is no longer listed. Learn why before the transaction opens, since
    // each is a read of the host.
    let mut gone: Vec<(Issue, &'static str)> = Vec::new();
    let mut unread: Vec<(crate::domain::intake::RepoRef, u64)> = Vec::new();
    if !truncated {
        let mut tx = env.store.begin(community).await?;
        for row in tx.issues().of_rule(rule.id).await? {
            if row.left.is_empty()
                && !matches
                    .iter()
                    .any(|m| m.repo == row.issue.repo && m.number == row.issue.number)
            {
                unread.push((row.issue.repo.clone(), row.issue.number));
            }
        }
    }
    for (index, (repo, number)) in unread.iter().enumerate() {
        if index >= MAX_DROPOUT_READS {
            break;
        }
        match host.get_issue(repo, *number).await {
            Ok(issue) => {
                let why = if issue.state == IssueState::Closed {
                    LEFT_CLOSED
                } else {
                    LEFT_UNMATCHED
                };
                gone.push((issue, why));
            }
            Err(HostError::NotFound(_)) => {}
            Err(other) => return Err(other.into()),
        }
    }

    let ctx = Ctx {
        community,
        actor: system(env),
        requester: None,
    };
    let mut outcome = Outcome {
        matched: matches.len(),
        truncated,
        ..Outcome::default()
    };
    let mut tx = env.store.begin(community).await?;
    // The rule may have been deleted while the host was read.
    if tx.intake().get(rule.id).await?.is_none() {
        return Ok((outcome, Vec::new()));
    }
    for issue in &matches {
        let row = tx.issues().upsert(issue, now).await?;
        match row.card_id {
            // A card that was removed is not made again: a person took it off the board.
            Some(_) if row.left.is_empty() => {}
            Some(_) => tx.issues().set_left(&issue.repo, issue.number, "").await?,
            None => {
                let card = insert_card(
                    &mut *tx,
                    &ctx,
                    NewCard {
                        title: card_title(issue),
                        description: card_description(issue),
                        workflow: rule.workflow.clone(),
                    },
                )
                .await?;
                tx.issues()
                    .attach(&issue.repo, issue.number, card.id, Some(rule.id))
                    .await?;
                outcome.added.push(card.id);
            }
        }
    }
    // Unread dropouts are taken as no longer matching.
    let mut left: Vec<(Issue, &'static str)> = gone;
    if !truncated {
        for (repo, number) in &unread {
            if !left
                .iter()
                .any(|(i, _)| i.repo == *repo && i.number == *number)
            {
                if let Some(row) = tx.issues().get(repo, *number).await? {
                    left.push((row.issue, LEFT_UNMATCHED));
                }
            }
        }
    }
    for (issue, why) in left {
        tx.issues().upsert(&issue, now).await?;
        tx.issues().set_left(&issue.repo, issue.number, why).await?;
        let row = tx.issues().get(&issue.repo, issue.number).await?;
        let card = match row.and_then(|r| r.card_id) {
            Some(id) => tx.cards().get(id).await?,
            None => None,
        };
        // A card a person removed has nothing to mark.
        if let Some(card) = card {
            tx.events()
                .append(Event::new(
                    "card.left_intake",
                    Some(card.id),
                    None,
                    ctx.actor.clone(),
                    json!({
                        "rule_id": rule.id,
                        "repo": issue.repo.to_string(),
                        "number": issue.number,
                        "status": why,
                    }),
                ))
                .await?;
            outcome.left += 1;
        }
    }
    tx.events()
        .append(Event::new(
            "intake.refreshed",
            None,
            None,
            ctx.actor.clone(),
            json!({
                "rule_id": rule.id,
                "repo": rule.repo.to_string(),
                "matched": outcome.matched,
                "added": outcome.added.len(),
                "left": outcome.left,
                "truncated": outcome.truncated,
            }),
        ))
        .await?;
    let events = tx.commit().await?;
    Ok((outcome, events))
}

/// Every page of the rule's open issues, up to [`MAX_PAGES`]. True when the repo had more.
async fn read_open(
    host: &dyn CodeHost,
    rule: &IntakeRule,
) -> Result<(Vec<Issue>, bool), HostError> {
    let filter = rule.filter.listing();
    let mut all = Vec::new();
    for number in 1..=MAX_PAGES {
        let page = host
            .list_issues(
                &rule.repo,
                &filter,
                Page {
                    number,
                    size: PAGE_SIZE,
                },
            )
            .await?;
        let full = page.len() >= PAGE_SIZE as usize;
        all.extend(page);
        if !full {
            return Ok((all, false));
        }
    }
    Ok((all, true))
}

/// Records that a pass failed, as `intake.failed`, with how many passes in a row have.
pub async fn record_failure(
    env: &Env<'_>,
    community: CommunityId,
    rule: Uuid,
    repo: &str,
    error: &str,
    failures: u32,
) -> Result<Vec<Event>, OpError> {
    let mut tx = env.store.begin(community).await?;
    tx.events()
        .append(Event::new(
            "intake.failed",
            None,
            None,
            system(env),
            json!({ "rule_id": rule, "repo": repo, "error": error, "failures": failures }),
        ))
        .await?;
    Ok(tx.commit().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_comes_one_interval_later_plus_up_to_a_fifth() {
        assert_eq!(next_wake(900, 0.0), Duration::from_secs(900));
        assert_eq!(next_wake(900, 0.5), Duration::from_secs(990));
        assert_eq!(next_wake(900, 1.0), Duration::from_secs(1080));
    }

    #[test]
    fn the_first_pass_starts_within_a_fifth_of_the_interval() {
        assert_eq!(start_delay(600, 0.0), Duration::ZERO);
        assert_eq!(start_delay(600, 1.0), Duration::from_secs(120));
    }

    #[test]
    fn failures_double_the_wait_up_to_the_cap() {
        assert_eq!(backoff(60, 0), Duration::from_secs(60));
        assert_eq!(backoff(60, 1), Duration::from_secs(120));
        assert_eq!(backoff(60, 3), Duration::from_secs(480));
        assert_eq!(backoff(60, 40), MAX_BACKOFF);
        assert_eq!(backoff(900, 1), Duration::from_secs(1800));
    }

    #[test]
    fn a_rate_limit_waits_what_the_host_asked_or_longer_up_to_the_cap() {
        assert_eq!(
            rate_wait(Duration::from_secs(120), 60, 1),
            Duration::from_secs(120)
        );
        assert_eq!(
            rate_wait(Duration::from_secs(10), 60, 1),
            Duration::from_secs(60)
        );
        assert_eq!(
            rate_wait(Duration::from_secs(10), 60, 3),
            Duration::from_secs(240)
        );
        assert_eq!(rate_wait(Duration::from_secs(86_400), 60, 1), MAX_BACKOFF);
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_holds_callers_until_the_time_the_host_named() {
        let gate = Gate::default();
        gate.hold(Duration::from_secs(90)).await;
        let start = Instant::now();
        gate.wait().await;
        assert_eq!(start.elapsed(), Duration::from_secs(90));
        // A later, shorter hold does not shorten the first.
        gate.hold(Duration::from_secs(10)).await;
        gate.hold(Duration::from_secs(5)).await;
        let start = Instant::now();
        gate.wait().await;
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }
}
