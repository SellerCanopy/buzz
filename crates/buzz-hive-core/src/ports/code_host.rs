//! The code host: where issues live. GitHub is the first adapter.
//!
//! A call may be repeated after a failure, so the calls that write take a key, and an adapter
//! makes a repeat harmless. Marking a pull request ready and merging it are harmless to repeat
//! by their nature, so they take no key.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::domain::card::PullRequest;
use crate::domain::intake::{Comment, CommentId, Issue, IssueFilter, Page, RepoRef};

/// Why a call to the host failed. A caller acts on the kind, not on the text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The repo or the issue is not there, or the token cannot see it.
    #[error("not found: {0}")]
    NotFound(String),
    /// The host asked callers to wait. Nothing was done.
    #[error("rate limited, try again in {retry_after:?}")]
    RateLimited { retry_after: Duration },
    /// The host refused the token, or there is none.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The host understood the call and refused it for what it asked, such as a pull request
    /// from a branch that was never pushed. Asking again changes nothing until the cause is fixed.
    #[error("rejected: {0}")]
    Rejected(String),
    /// Anything else: the host is down, a reply could not be read, a request timed out.
    #[error("{0}")]
    Other(String),
}

#[async_trait::async_trait]
pub trait CodeHost: Send + Sync {
    /// One page of `repo`'s issues, newest update first. Pull requests are not issues.
    async fn list_issues(
        &self,
        repo: &RepoRef,
        filter: &IssueFilter,
        page: Page,
    ) -> Result<Vec<Issue>, HostError>;

    async fn get_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, HostError>;

    /// One page of the issues a search in the host's own syntax finds.
    async fn search_issues(&self, query: &str, page: Page) -> Result<Vec<Issue>, HostError>;

    /// The comments on an issue made after `since`, oldest first, or all of them.
    async fn list_comments(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Comment>, HostError>;

    /// Writes a comment that carries `key`. A comment already carrying `key` is not written
    /// again, and its id is returned.
    async fn comment(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
        key: &str,
    ) -> Result<CommentId, HostError>;

    /// Opens a pull request, or returns the open one that already has `request.head` as its
    /// head, so a repeat after a failure opens one pull request. `key` names the call for
    /// an adapter that can remember it. `Rejected` means the host refused the request itself,
    /// for example because the head branch is not on the host.
    async fn open_pull_request(
        &self,
        request: &NewPullRequest,
        key: &str,
    ) -> Result<PullRequest, HostError>;

    /// The pull request as the host has it now, with its labels. A pull request the token
    /// cannot see is `NotFound`.
    async fn get_pull_request(&self, repo: &RepoRef, number: u64) -> Result<PullStatus, HostError>;

    /// The checks of the pull request's head commit, one per name, with the commit they are
    /// of. A head with none gives an empty list.
    async fn list_checks(&self, repo: &RepoRef, number: u64) -> Result<PullChecks, HostError>;

    /// Takes a draft pull request out of draft. One that is ready already is left as it is.
    async fn mark_ready(&self, repo: &RepoRef, number: u64) -> Result<(), HostError>;

    /// Merges the pull request. One that is merged already is left as it is. With `head_sha`,
    /// the host merges only if the pull request's head is still that commit, so what was
    /// checked is what is merged. `Rejected` means the host refused: a head that moved, a
    /// conflict, a failing required check, a branch rule, or a closed pull request.
    async fn merge(
        &self,
        repo: &RepoRef,
        number: u64,
        method: MergeMethod,
        head_sha: Option<&str>,
    ) -> Result<(), HostError>;

    /// The repos the token reaches under `owner`, or every repo it reaches when empty.
    async fn list_repos(&self, owner: &str) -> Result<Vec<RepoRef>, HostError>;

    /// The token's own login, then the login of each organization it belongs to.
    async fn list_owners(&self) -> Result<Vec<String>, HostError>;
}

/// What a pull request is opened with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPullRequest {
    pub repo: RepoRef,
    /// The branch the work is on.
    pub head: String,
    /// The branch to merge into. Empty means the repo's default branch.
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// A pull request as the host reports it now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullStatus {
    /// Its state, whether a reviewer approved it, and the rest of what Beehive keeps. `checks`
    /// is empty: the checks are read by `list_checks`.
    pub pull: PullRequest,
    pub labels: Vec<String>,
}

/// The checks of a pull request's head commit, and the commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullChecks {
    /// The head commit the checks are of.
    pub head_sha: String,
    pub checks: Vec<Check>,
}

/// One check of a pull request's head commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    /// Queued or running.
    Pending,
    /// Succeeded, was skipped, or finished neutral.
    Passed,
    /// Failed, timed out, was cancelled, or needs action.
    Failed,
}

/// How the host merges a pull request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Merge,
    #[default]
    Squash,
    Rebase,
}

impl MergeMethod {
    /// The method a `merge` action's `method` argument names. An empty one is the default.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "" => Some(Self::default()),
            "merge" => Some(Self::Merge),
            "squash" => Some(Self::Squash),
            "rebase" => Some(Self::Rebase),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}

/// The most repos `list_repos` returns. A caller that gets this many knows the list may be cut.
pub const MAX_REPOS: usize = 500;
