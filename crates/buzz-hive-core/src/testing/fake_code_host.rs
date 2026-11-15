use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::domain::intake::{Comment, CommentId, Issue, IssueFilter, IssueState, Page, RepoRef};
use crate::ports::code_host::{CodeHost, HostError};

#[derive(Default)]
struct Inner {
    issues: Vec<Issue>,
    /// Every comment with the issue it is on and the key it was written under, if any.
    comments: Vec<(RepoRef, u64, Comment, Option<String>)>,
    repos: Vec<RepoRef>,
    owners: Vec<String>,
    /// Errors the next calls fail with, one per call, oldest first.
    failures: VecDeque<HostError>,
    next_comment: CommentId,
    /// Every call made, as a line such as `list_issues example-org/api page 1`.
    calls: Vec<String>,
}

/// A [`CodeHost`] held in memory. Calls are recorded, and a test can make the next ones fail.
#[derive(Clone, Default)]
pub struct FakeCodeHost {
    inner: Arc<Mutex<Inner>>,
}

impl FakeCodeHost {
    pub fn new() -> Self {
        let host = Self::default();
        host.inner.lock().unwrap().next_comment = 1000;
        host
    }

    /// Adds an issue, or replaces the one with the same repo and number.
    pub fn put_issue(&self, issue: Issue) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .issues
            .retain(|have| !(have.repo == issue.repo && have.number == issue.number));
        inner.issues.push(issue);
    }

    pub fn remove_issue(&self, repo: &RepoRef, number: u64) {
        self.inner
            .lock()
            .unwrap()
            .issues
            .retain(|have| !(have.repo == *repo && have.number == number));
    }

    /// Adds a comment someone else wrote. Returns its id.
    pub fn put_comment(
        &self,
        repo: &RepoRef,
        number: u64,
        author: &str,
        body: &str,
        at: DateTime<Utc>,
    ) -> CommentId {
        let mut inner = self.inner.lock().unwrap();
        inner.next_comment += 1;
        let id = inner.next_comment;
        let comment = Comment {
            id,
            author: author.to_owned(),
            body: body.to_owned(),
            created_at: at,
            from_hive: false,
        };
        inner.comments.push((repo.clone(), number, comment, None));
        id
    }

    pub fn put_repo(&self, repo: &RepoRef) {
        self.inner.lock().unwrap().repos.push(repo.clone());
    }

    pub fn set_owners(&self, owners: &[&str]) {
        self.inner.lock().unwrap().owners = owners.iter().map(|o| (*o).to_owned()).collect();
    }

    /// The next call fails with `error`. Calls queue: the one after fails with the next.
    pub fn fail_next(&self, error: HostError) {
        self.inner.lock().unwrap().failures.push_back(error);
    }

    /// Every call made so far, oldest first.
    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// The comments Beehive wrote, as `(repo, number, body)`.
    pub fn written(&self) -> Vec<(RepoRef, u64, String)> {
        self.inner
            .lock()
            .unwrap()
            .comments
            .iter()
            .filter(|(.., key)| key.is_some())
            .map(|(repo, number, comment, _)| (repo.clone(), *number, comment.body.clone()))
            .collect()
    }

    /// Records `call` and returns the failure queued for it, if any.
    fn enter(&self, call: String) -> Result<std::sync::MutexGuard<'_, Inner>, HostError> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(call);
        match inner.failures.pop_front() {
            Some(error) => Err(error),
            None => Ok(inner),
        }
    }
}

fn paged(mut found: Vec<Issue>, page: Page) -> Vec<Issue> {
    found.sort_by_key(|issue| std::cmp::Reverse(issue.number));
    let skip = (page.number.saturating_sub(1) * page.size) as usize;
    found
        .into_iter()
        .skip(skip)
        .take(page.size as usize)
        .collect()
}

#[async_trait::async_trait]
impl CodeHost for FakeCodeHost {
    async fn list_issues(
        &self,
        repo: &RepoRef,
        filter: &IssueFilter,
        page: Page,
    ) -> Result<Vec<Issue>, HostError> {
        let inner = self.enter(format!("list_issues {repo} page {}", page.number))?;
        if !inner.issues.iter().any(|i| i.repo == *repo) && !inner.repos.contains(repo) {
            return Err(HostError::NotFound(format!("{repo} does not exist")));
        }
        let found = inner
            .issues
            .iter()
            .filter(|i| i.repo == *repo && i.state == filter.state)
            .filter(|i| filter.label.as_ref().is_none_or(|l| i.labels.contains(l)))
            .filter(|i| !filter.unassigned || i.assignees.is_empty())
            .cloned()
            .collect();
        Ok(paged(found, page))
    }

    async fn get_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, HostError> {
        let inner = self.enter(format!("get_issue {repo}#{number}"))?;
        inner
            .issues
            .iter()
            .find(|i| i.repo == *repo && i.number == number)
            .cloned()
            .ok_or_else(|| HostError::NotFound(format!("{repo}#{number} does not exist")))
    }

    async fn search_issues(&self, query: &str, page: Page) -> Result<Vec<Issue>, HostError> {
        let inner = self.enter(format!("search_issues {query} page {}", page.number))?;
        let mut repos = Vec::new();
        let mut state = None;
        let mut label = None;
        let mut unassigned = false;
        let mut words = Vec::new();
        for term in query.split_whitespace() {
            if let Some(repo) = term.strip_prefix("repo:") {
                repos.extend(RepoRef::parse(repo).ok());
            } else if let Some(s) = term.strip_prefix("state:") {
                state = IssueState::parse(s);
            } else if let Some(l) = term.strip_prefix("label:") {
                label = Some(l.trim_matches('"').to_owned());
            } else if term == "no:assignee" {
                unassigned = true;
            } else if term != "is:issue" {
                words.push(term.to_lowercase());
            }
        }
        let found = inner
            .issues
            .iter()
            .filter(|i| repos.contains(&i.repo))
            .filter(|i| state.is_none_or(|s| i.state == s))
            .filter(|i| label.as_ref().is_none_or(|l| i.labels.contains(l)))
            .filter(|i| !unassigned || i.assignees.is_empty())
            .filter(|i| {
                let text = format!("{} {}", i.title, i.body).to_lowercase();
                words.iter().all(|w| text.contains(w))
            })
            .cloned()
            .collect();
        Ok(paged(found, page))
    }

    async fn list_comments(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Comment>, HostError> {
        let inner = self.enter(format!("list_comments {repo}#{number}"))?;
        let mut found: Vec<Comment> = inner
            .comments
            .iter()
            .filter(|(r, n, c, _)| {
                r == repo && *n == number && since.is_none_or(|s| c.created_at > s)
            })
            .map(|(_, _, c, _)| c.clone())
            .collect();
        found.sort_by_key(|c| (c.created_at, c.id));
        Ok(found)
    }

    async fn comment(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
        key: &str,
    ) -> Result<CommentId, HostError> {
        let mut inner = self.enter(format!("comment {repo}#{number} {key}"))?;
        if let Some((.., comment, _)) = inner
            .comments
            .iter()
            .find(|(r, n, _, k)| r == repo && *n == number && k.as_deref() == Some(key))
        {
            return Ok(comment.id);
        }
        inner.next_comment += 1;
        let id = inner.next_comment;
        let comment = Comment {
            id,
            author: "beehive".into(),
            body: body.to_owned(),
            created_at: Utc::now(),
            from_hive: true,
        };
        inner
            .comments
            .push((repo.clone(), number, comment, Some(key.to_owned())));
        Ok(id)
    }

    async fn list_repos(&self, owner: &str) -> Result<Vec<RepoRef>, HostError> {
        let inner = self.enter(format!("list_repos {owner}"))?;
        Ok(inner
            .repos
            .iter()
            .filter(|r| owner.is_empty() || r.owner == owner)
            .cloned()
            .collect())
    }

    async fn list_owners(&self) -> Result<Vec<String>, HostError> {
        Ok(self.enter("list_owners".into())?.owners.clone())
    }
}
