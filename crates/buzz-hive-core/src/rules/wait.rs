//! What a stage that waits for outside state is waiting for, and whether the state it waits for
//! has come.
//!
//! A `wait` stage, the `wait` action and the `wait_for_ci` action have no agent. The pull
//! request sync reads the host at the stage's poll and asks [`poll_decision`] whether to pass,
//! fail or keep waiting. Ported from the Go engine's wait kind and `wait_for_ci` action.

use std::time::Duration;

use crate::domain::card::PullRequest;
use crate::domain::workflow::{
    ActionArg, Duration as SettingDuration, Stage, KIND_ACTION, KIND_WAIT,
};
use crate::ports::code_host::{Check, CheckState};
use crate::workflow::actions::{WAIT, WAIT_FOR_CI};

/// How often a waiting stage reads the host when it does not say.
pub const DEFAULT_POLL: Duration = Duration::from_secs(60);
/// The shortest poll. The sync looks for due stages every ten seconds, so a shorter poll would
/// not be kept.
pub const MIN_POLL: Duration = Duration::from_secs(10);
/// How long `wait_for_ci` waits before it fails, when it does not say.
pub const DEFAULT_CI_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// How long `wait_for_ci` lets the checks appear, when it does not say. A pull request just
/// opened has none for a moment, and a repo with no CI never gets any.
pub const DEFAULT_CI_GRACE: Duration = Duration::from_secs(2 * 60);

/// The pull request state a wait stage waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullWait {
    Approved,
    Merged,
}

/// The checks `wait_for_ci` waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiWait {
    /// The checks that must pass, by name. Empty means every check the host reports.
    pub required: Vec<String>,
    /// How long checks may take to appear.
    pub grace: Duration,
}

/// A waiting stage's conditions and timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    /// Labels the card's issue or pull request must carry.
    pub labels: Vec<String>,
    pub pull_request: Option<PullWait>,
    pub ci: Option<CiWait>,
    pub poll: Duration,
    /// Zero means it waits for as long as it takes.
    pub timeout: Duration,
}

impl Waiting {
    /// Whether the stage cannot be decided without the card's pull request.
    pub fn needs_pull_request(&self) -> bool {
        self.pull_request.is_some() || self.ci.is_some()
    }

    /// What the stage waits for, in a sentence for the card page.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.labels.is_empty() {
            parts.push(format!("the labels {}", self.labels.join(", ")));
        }
        match self.pull_request {
            Some(PullWait::Approved) => parts.push("the pull request to be approved".into()),
            Some(PullWait::Merged) => parts.push("the pull request to be merged".into()),
            None => {}
        }
        if let Some(ci) = &self.ci {
            parts.push(if ci.required.is_empty() {
                "the checks to pass".to_owned()
            } else {
                format!("the checks {} to pass", ci.required.join(", "))
            });
        }
        format!("waiting for {}", parts.join(" and "))
    }
}

fn parse(text: &str) -> Option<Duration> {
    let nanos = SettingDuration::parse(text).ok()?.nanos();
    u64::try_from(nanos)
        .ok()
        .filter(|n| *n > 0)
        .map(Duration::from_nanos)
}

fn list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn pull_wait(text: &str) -> Option<PullWait> {
    match text.trim() {
        "approved" => Some(PullWait::Approved),
        "merged" => Some(PullWait::Merged),
        _ => None,
    }
}

/// What `stage` waits for, or `None` for a stage that does not wait on outside state: a wait
/// stage, the `wait` action or `wait_for_ci`.
pub fn waiting_of(stage: &Stage) -> Option<Waiting> {
    let arg = |name: &str| {
        stage
            .with
            .as_ref()
            .and_then(|with| with.get(name))
            .map_or("", ActionArg::text)
    };
    let poll = |text: &str| parse(text).unwrap_or(DEFAULT_POLL).max(MIN_POLL);
    if stage.kind == KIND_WAIT {
        let wait = stage.wait.as_ref()?;
        let timeout = u64::try_from(wait.timeout.nanos()).unwrap_or(0);
        let polled = u64::try_from(wait.poll.nanos()).unwrap_or(0);
        return Some(Waiting {
            labels: wait.labels.clone(),
            pull_request: pull_wait(&wait.pull_request),
            ci: None,
            poll: if polled == 0 {
                DEFAULT_POLL
            } else {
                Duration::from_nanos(polled).max(MIN_POLL)
            },
            timeout: Duration::from_nanos(timeout),
        });
    }
    if stage.kind != KIND_ACTION {
        return None;
    }
    match stage.action.as_str() {
        WAIT => Some(Waiting {
            labels: list(arg("labels")),
            pull_request: pull_wait(arg("pull_request")),
            ci: None,
            poll: poll(arg("poll")),
            timeout: parse(arg("timeout")).unwrap_or_default(),
        }),
        WAIT_FOR_CI => Some(Waiting {
            labels: Vec::new(),
            pull_request: None,
            ci: Some(CiWait {
                required: list(arg("required")),
                grace: parse(arg("grace")).unwrap_or(DEFAULT_CI_GRACE),
            }),
            poll: poll(arg("poll")),
            timeout: parse(arg("timeout")).unwrap_or(DEFAULT_CI_TIMEOUT),
        }),
        _ => None,
    }
}

/// What a read of the host says about a waiting stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Passed,
    /// The stage fails, for this reason.
    Failed(String),
    Pending,
}

/// Decides a waiting stage. `pr` is the card's pull request as the host has it, or `None` for a
/// card with none. `labels` are the labels of the card's issue and the pull request, `checks`
/// the pull request's checks, and `waited` how long the stage has been entered.
///
/// A closed pull request fails the stage, and a merged one passes it: whatever it waited for
/// no longer matters. A stage that names labels and a pull request state waits for both.
pub fn poll_decision(
    wait: &Waiting,
    pr: Option<&PullRequest>,
    labels: &[String],
    checks: &[Check],
    waited: Duration,
) -> WaitOutcome {
    if let Some(pr) = pr.filter(|_| wait.needs_pull_request()) {
        match pr.state.as_str() {
            "merged" => return WaitOutcome::Passed,
            "closed" => return WaitOutcome::Failed("the pull request was closed".into()),
            _ => {}
        }
    }
    if !wait.labels.iter().all(|label| labels.contains(label)) {
        return WaitOutcome::Pending;
    }
    match (wait.pull_request, pr) {
        (Some(PullWait::Approved), Some(pr)) if !pr.approved => return WaitOutcome::Pending,
        (Some(_), None) => return WaitOutcome::Pending,
        (Some(PullWait::Merged), Some(_)) => return WaitOutcome::Pending,
        _ => {}
    }
    match &wait.ci {
        Some(ci) if pr.is_some() => ci_decision(ci, checks, waited),
        Some(_) => WaitOutcome::Pending,
        None => WaitOutcome::Passed,
    }
}

fn ci_decision(ci: &CiWait, checks: &[Check], waited: Duration) -> WaitOutcome {
    let counted: Vec<&Check> = checks
        .iter()
        .filter(|c| ci.required.is_empty() || ci.required.contains(&c.name))
        .collect();
    let red: Vec<&str> = counted
        .iter()
        .filter(|c| c.state == CheckState::Failed)
        .map(|c| c.name.as_str())
        .collect();
    if !red.is_empty() {
        return WaitOutcome::Failed(format!(
            "{} {} failed",
            if red.len() == 1 {
                "the check"
            } else {
                "the checks"
            },
            red.join(", ")
        ));
    }
    let missing: Vec<&str> = ci
        .required
        .iter()
        .filter(|name| !checks.iter().any(|c| &c.name == *name))
        .map(String::as_str)
        .collect();
    let late = waited >= ci.grace;
    if !missing.is_empty() {
        return if late {
            WaitOutcome::Failed(format!(
                "the required checks {} never reported",
                missing.join(", ")
            ))
        } else {
            WaitOutcome::Pending
        };
    }
    if counted.iter().any(|c| c.state == CheckState::Pending) {
        return WaitOutcome::Pending;
    }
    // A repo with no checks has nothing to wait for once the grace has passed.
    if counted.is_empty() && !late {
        return WaitOutcome::Pending;
    }
    WaitOutcome::Passed
}

/// How a set of checks reads on the card: `passing`, `failing`, `pending`, or empty for none.
pub fn summarize(checks: &[Check]) -> &'static str {
    if checks.iter().any(|c| c.state == CheckState::Failed) {
        "failing"
    } else if checks.iter().any(|c| c.state == CheckState::Pending) {
        "pending"
    } else if checks.is_empty() {
        ""
    } else {
        "passing"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, state: CheckState) -> Check {
        Check {
            name: name.into(),
            state,
        }
    }

    fn pull(state: &str, approved: bool) -> PullRequest {
        PullRequest {
            state: state.into(),
            approved,
            ..PullRequest::default()
        }
    }

    fn ci(required: &[&str]) -> Waiting {
        Waiting {
            labels: vec![],
            pull_request: None,
            ci: Some(CiWait {
                required: required.iter().map(|s| (*s).to_owned()).collect(),
                grace: Duration::from_secs(120),
            }),
            poll: DEFAULT_POLL,
            timeout: DEFAULT_CI_TIMEOUT,
        }
    }

    const NOW: Duration = Duration::from_secs(1);
    const LATE: Duration = Duration::from_secs(600);

    #[test]
    fn ci_passes_when_every_check_is_green() {
        let checks = [
            check("build", CheckState::Passed),
            check("lint", CheckState::Passed),
        ];
        let open = pull("open", false);
        assert_eq!(
            poll_decision(&ci(&[]), Some(&open), &[], &checks, NOW),
            WaitOutcome::Passed
        );
    }

    #[test]
    fn ci_fails_naming_the_red_required_checks_and_ignores_the_others() {
        let checks = [
            check("build", CheckState::Failed),
            check("lint", CheckState::Failed),
            check("docs", CheckState::Failed),
        ];
        let open = pull("open", false);
        assert_eq!(
            poll_decision(&ci(&["build", "lint"]), Some(&open), &[], &checks, NOW),
            WaitOutcome::Failed("the checks build, lint failed".into())
        );
        let only_docs = [
            check("build", CheckState::Passed),
            check("docs", CheckState::Failed),
        ];
        assert_eq!(
            poll_decision(&ci(&["build"]), Some(&open), &[], &only_docs, NOW),
            WaitOutcome::Passed
        );
    }

    #[test]
    fn ci_waits_for_a_pending_check_and_for_checks_to_appear() {
        let open = pull("open", false);
        let running = [check("build", CheckState::Pending)];
        assert_eq!(
            poll_decision(&ci(&[]), Some(&open), &[], &running, LATE),
            WaitOutcome::Pending
        );
        assert_eq!(
            poll_decision(&ci(&[]), Some(&open), &[], &[], NOW),
            WaitOutcome::Pending
        );
        // A repo with no CI passes once the grace is over.
        assert_eq!(
            poll_decision(&ci(&[]), Some(&open), &[], &[], LATE),
            WaitOutcome::Passed
        );
    }

    #[test]
    fn a_required_check_that_never_reports_fails_after_the_grace() {
        let open = pull("open", false);
        let checks = [check("lint", CheckState::Passed)];
        assert_eq!(
            poll_decision(&ci(&["build"]), Some(&open), &[], &checks, NOW),
            WaitOutcome::Pending
        );
        assert_eq!(
            poll_decision(&ci(&["build"]), Some(&open), &[], &checks, LATE),
            WaitOutcome::Failed("the required checks build never reported".into())
        );
    }

    #[test]
    fn a_closed_pull_request_fails_and_a_merged_one_passes() {
        let wait = Waiting {
            labels: vec![],
            pull_request: Some(PullWait::Approved),
            ci: None,
            poll: DEFAULT_POLL,
            timeout: Duration::ZERO,
        };
        assert_eq!(
            poll_decision(&wait, Some(&pull("closed", true)), &[], &[], NOW),
            WaitOutcome::Failed("the pull request was closed".into())
        );
        assert_eq!(
            poll_decision(&wait, Some(&pull("merged", false)), &[], &[], NOW),
            WaitOutcome::Passed
        );
        assert_eq!(
            poll_decision(&ci(&[]), Some(&pull("closed", false)), &[], &[], NOW),
            WaitOutcome::Failed("the pull request was closed".into())
        );
    }

    #[test]
    fn approval_and_merge_waits() {
        let approved = Waiting {
            labels: vec![],
            pull_request: Some(PullWait::Approved),
            ci: None,
            poll: DEFAULT_POLL,
            timeout: Duration::ZERO,
        };
        assert_eq!(
            poll_decision(&approved, Some(&pull("open", false)), &[], &[], NOW),
            WaitOutcome::Pending
        );
        assert_eq!(
            poll_decision(&approved, Some(&pull("open", true)), &[], &[], NOW),
            WaitOutcome::Passed
        );
        assert_eq!(
            poll_decision(&approved, None, &[], &[], NOW),
            WaitOutcome::Pending
        );
        let merged = Waiting {
            pull_request: Some(PullWait::Merged),
            ..approved
        };
        assert_eq!(
            poll_decision(&merged, Some(&pull("open", true)), &[], &[], NOW),
            WaitOutcome::Pending
        );
    }

    #[test]
    fn labels_and_a_pull_request_state_are_both_needed() {
        let wait = Waiting {
            labels: vec!["qa".into(), "ship".into()],
            pull_request: Some(PullWait::Approved),
            ci: None,
            poll: DEFAULT_POLL,
            timeout: Duration::ZERO,
        };
        let approved = pull("open", true);
        let one = ["qa".to_owned()];
        let both = ["qa".to_owned(), "ship".to_owned()];
        assert_eq!(
            poll_decision(&wait, Some(&approved), &one, &[], NOW),
            WaitOutcome::Pending
        );
        assert_eq!(
            poll_decision(&wait, Some(&approved), &both, &[], NOW),
            WaitOutcome::Passed
        );
        assert_eq!(
            poll_decision(&wait, Some(&pull("open", false)), &both, &[], NOW),
            WaitOutcome::Pending
        );
    }

    #[test]
    fn labels_alone_need_no_pull_request() {
        let wait = Waiting {
            labels: vec!["go".into()],
            pull_request: None,
            ci: None,
            poll: DEFAULT_POLL,
            timeout: Duration::ZERO,
        };
        assert_eq!(
            poll_decision(&wait, None, &["go".to_owned()], &[], NOW),
            WaitOutcome::Passed
        );
        assert_eq!(
            poll_decision(&wait, None, &[], &[], NOW),
            WaitOutcome::Pending
        );
    }

    #[test]
    fn checks_summarize_by_the_worst_one() {
        assert_eq!(summarize(&[]), "");
        let green = check("a", CheckState::Passed);
        let wait = check("b", CheckState::Pending);
        let red = check("c", CheckState::Failed);
        assert_eq!(summarize(std::slice::from_ref(&green)), "passing");
        assert_eq!(summarize(&[green.clone(), wait.clone()]), "pending");
        assert_eq!(summarize(&[green, wait, red]), "failing");
    }
}
