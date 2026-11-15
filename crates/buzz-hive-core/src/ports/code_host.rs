//! The code host: where issues live. GitHub is the first adapter.
//!
//! A call may be repeated after a failure, so the calls that write take a key, and an adapter
//! makes a repeat harmless. Later phases add the calls for pull requests and checks.

use std::time::Duration;

use chrono::{DateTime, Utc};

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

    /// The repos the token reaches under `owner`, or every repo it reaches when empty.
    async fn list_repos(&self, owner: &str) -> Result<Vec<RepoRef>, HostError>;

    /// The token's own login, then the login of each organization it belongs to.
    async fn list_owners(&self) -> Result<Vec<String>, HostError>;
}

/// The most repos `list_repos` returns. A caller that gets this many knows the list may be cut.
pub const MAX_REPOS: usize = 500;
