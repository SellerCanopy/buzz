//! The GitHub adapter of [`CodeHost`], over the REST API with one token for the whole team.
//!
//! Every read is conditional: the adapter keeps the entity tag and body of each answer it got,
//! sends the tag back as `If-None-Match`, and takes a `304` as "unchanged". GitHub does not
//! count a `304` against the rate limit, so a rule that finds nothing new costs nothing.
//! The cache is in memory and is lost on a restart; the first pass after one reads in full.
//!
//! The token goes in one header and is never logged or put in an error. Text that comes back
//! (issue titles, comments) is data: this adapter parses it and passes it on, nothing more.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use buzz_hive_core::domain::card::PullRequest;
use buzz_hive_core::domain::intake::{
    Comment, CommentId, Issue, IssueFilter, IssueState, Page, RepoRef,
};
use buzz_hive_core::ports::code_host::{CodeHost, HostError, NewPullRequest, MAX_REPOS};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ETAG, IF_NONE_MATCH, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use serde_json::{json, Value};

/// The REST API of github.com.
pub const DEFAULT_API_URL: &str = "https://api.github.com";

/// Entries the etag cache holds. When it is full it is emptied, which only costs reads.
const CACHE_ENTRIES: usize = 256;
/// A body larger than this is read but not kept for the next request.
const CACHE_BODY_BYTES: usize = 2 * 1024 * 1024;
/// The largest answer read.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Pages of comments read for one issue.
const MAX_COMMENT_PAGES: u32 = 10;
/// Comments per page, the most GitHub allows.
const COMMENT_PAGE: u32 = 100;
/// How long a limit with no stated end is waited out.
const UNSTATED_LIMIT: Duration = Duration::from_secs(60);
/// The line a comment Beehive wrote ends with, so it is known again. The key is made safe
/// for an HTML comment.
const MARKER_PREFIX: &str = "<!-- buzz-hive:";
const MARKER_SUFFIX: &str = " -->";

type Cached = (String, Arc<Value>);

pub struct GitHub {
    client: reqwest::Client,
    base: Url,
    cache: Mutex<HashMap<String, Cached>>,
}

impl GitHub {
    /// A client for the API at `api_url` that signs in with `token`.
    pub fn new(api_url: &str, token: &str) -> Result<Self, String> {
        let base = Url::parse(api_url.trim_end_matches('/'))
            .map_err(|e| format!("the GitHub API address is not a URL ({e})"))?;
        if !matches!(base.scheme(), "http" | "https") || base.cannot_be_a_base() {
            return Err("the GitHub API address must be an http or https URL".into());
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err("the GitHub API address must have no query or fragment".into());
        }
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.trim()))
            .map_err(|_| "GH_TOKEN has a character a header cannot carry".to_owned())?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, auth);
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            HeaderName::from_static("x-github-api-version"),
            HeaderValue::from_static("2022-11-28"),
        );
        let client = reqwest::Client::builder()
            .user_agent("buzz-hive")
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("cannot build the GitHub client: {e}"))?;
        Ok(Self {
            client,
            base,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The URL of `segments` under the API address, each one escaped, with `query`.
    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(segments);
        }
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        url
    }

    /// A conditional GET. A `304` returns the body kept from the last `200` for the URL.
    async fn get(&self, url: Url) -> Result<(Arc<Value>, Option<String>), HostError> {
        let key = url.to_string();
        let cached = self.cache.lock().unwrap().get(&key).cloned();
        let mut request = self.client.get(url);
        if let Some((etag, _)) = &cached {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().await.map_err(transport)?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED {
            return match cached {
                Some((etag, body)) => Ok((body, Some(etag))),
                None => Err(HostError::Other(
                    "GitHub said not modified to a request that sent no etag".into(),
                )),
            };
        }
        let headers = response.headers().clone();
        let bytes = read_body(response).await?;
        if !status.is_success() {
            return Err(classify(status, &headers, &bytes));
        }
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|e| HostError::Other(format!("GitHub sent a reply that is not JSON ({e})")))?;
        let body = Arc::new(body);
        let etag = headers
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if let Some(etag) = &etag {
            if bytes.len() <= CACHE_BODY_BYTES {
                let mut cache = self.cache.lock().unwrap();
                if cache.len() >= CACHE_ENTRIES {
                    cache.clear();
                }
                cache.insert(key, (etag.clone(), body.clone()));
            }
        }
        Ok((body, etag))
    }

    async fn post(&self, url: Url, body: &Value) -> Result<Value, HostError> {
        let response = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = read_body(response).await?;
        if !status.is_success() {
            return Err(classify(status, &headers, &bytes));
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| HostError::Other(format!("GitHub sent a reply that is not JSON ({e})")))
    }

    /// The open pull request whose head is the branch `head` of `repo`, if there is one.
    async fn open_pull_for(
        &self,
        repo: &RepoRef,
        head: &str,
    ) -> Result<Option<PullRequest>, HostError> {
        let url = self.url(
            &["repos", &repo.owner, &repo.name, "pulls"],
            &[
                ("state", "open".to_owned()),
                ("head", format!("{}:{head}", repo.owner)),
                ("per_page", "1".to_owned()),
            ],
        );
        let (body, _) = self.get(url).await?;
        Ok(items(&body).first().and_then(|item| pull_from(item, repo)))
    }

    /// Every comment on an issue, oldest first, up to [`MAX_COMMENT_PAGES`] pages.
    async fn all_comments(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Comment>, HostError> {
        let number = number.to_string();
        let mut found = Vec::new();
        for page in 1..=MAX_COMMENT_PAGES {
            let mut query = vec![
                ("per_page", COMMENT_PAGE.to_string()),
                ("page", page.to_string()),
            ];
            if let Some(since) = since {
                query.push(("since", since.to_rfc3339_opts(SecondsFormat::Secs, true)));
            }
            let url = self.url(
                &[
                    "repos",
                    &repo.owner,
                    &repo.name,
                    "issues",
                    &number,
                    "comments",
                ],
                &query,
            );
            let (body, _) = self.get(url).await?;
            let items = body.as_array().map(Vec::as_slice).unwrap_or_default();
            found.extend(items.iter().filter_map(comment_from));
            if items.len() < COMMENT_PAGE as usize {
                break;
            }
        }
        Ok(found)
    }
}

#[async_trait::async_trait]
impl CodeHost for GitHub {
    async fn list_issues(
        &self,
        repo: &RepoRef,
        filter: &IssueFilter,
        page: Page,
    ) -> Result<Vec<Issue>, HostError> {
        let mut query = vec![
            ("state", filter.state.as_str().to_owned()),
            ("sort", "updated".to_owned()),
            ("direction", "desc".to_owned()),
            ("per_page", page.size.min(100).to_string()),
            ("page", page.number.to_string()),
        ];
        if let Some(label) = &filter.label {
            query.push(("labels", label.clone()));
        }
        if filter.unassigned {
            query.push(("assignee", "none".to_owned()));
        }
        let url = self.url(&["repos", &repo.owner, &repo.name, "issues"], &query);
        let (body, _) = self.get(url).await?;
        Ok(items(&body)
            .iter()
            .filter_map(|item| issue_from(item, Some(repo), None))
            .collect())
    }

    async fn get_issue(&self, repo: &RepoRef, number: u64) -> Result<Issue, HostError> {
        let number_text = number.to_string();
        let url = self.url(
            &["repos", &repo.owner, &repo.name, "issues", &number_text],
            &[],
        );
        let (body, etag) = self.get(url).await?;
        issue_from(&body, Some(repo), etag)
            .ok_or_else(|| HostError::NotFound(format!("{repo}#{number} is not an issue")))
    }

    async fn search_issues(&self, query: &str, page: Page) -> Result<Vec<Issue>, HostError> {
        let url = self.url(
            &["search", "issues"],
            &[
                ("q", query.to_owned()),
                ("per_page", page.size.min(100).to_string()),
                ("page", page.number.to_string()),
            ],
        );
        let (body, _) = self.get(url).await?;
        let found = body.get("items").map(items).unwrap_or_default();
        Ok(found
            .iter()
            .filter_map(|item| issue_from(item, None, None))
            .collect())
    }

    async fn list_comments(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Comment>, HostError> {
        self.all_comments(repo, number, since).await
    }

    async fn comment(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
        key: &str,
    ) -> Result<CommentId, HostError> {
        let marker = marker(key);
        // A repeat after a failure finds the comment the first try wrote.
        for existing in self.all_comments(repo, number, None).await? {
            if existing.from_hive && existing_has(&existing, &marker) {
                return Ok(existing.id);
            }
        }
        let number_text = number.to_string();
        let url = self.url(
            &[
                "repos",
                &repo.owner,
                &repo.name,
                "issues",
                &number_text,
                "comments",
            ],
            &[],
        );
        let written = self
            .post(url, &json!({ "body": format!("{body}\n\n{marker}") }))
            .await?;
        written
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| HostError::Other("GitHub's reply to a comment has no id".into()))
    }

    async fn open_pull_request(
        &self,
        request: &NewPullRequest,
        _key: &str,
    ) -> Result<PullRequest, HostError> {
        let repo = &request.repo;
        // A repeat after a failure, or a branch that already has its pull request.
        if let Some(found) = self.open_pull_for(repo, &request.head).await? {
            return Ok(found);
        }
        let base = if request.base.is_empty() {
            let (info, _) = self
                .get(self.url(&["repos", &repo.owner, &repo.name], &[]))
                .await?;
            info.get("default_branch")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    HostError::Other("GitHub's reply for the repo has no default branch".into())
                })?
        } else {
            request.base.clone()
        };
        let url = self.url(&["repos", &repo.owner, &repo.name, "pulls"], &[]);
        let made = self
            .post(
                url,
                &json!({
                    "title": request.title,
                    "head": request.head,
                    "base": base,
                    "body": request.body,
                    "draft": request.draft,
                }),
            )
            .await;
        match made {
            Ok(item) => pull_from(&item, repo).ok_or_else(|| {
                HostError::Other("GitHub's reply to a pull request has no number".into())
            }),
            // Two calls that raced: the other one's pull request is the one.
            Err(HostError::Rejected(message)) if message.contains("already exists") => self
                .open_pull_for(repo, &request.head)
                .await?
                .ok_or(HostError::Rejected(message)),
            Err(other) => Err(other),
        }
    }

    async fn list_repos(&self, owner: &str) -> Result<Vec<RepoRef>, HostError> {
        let mut repos = Vec::new();
        let pages = MAX_REPOS.div_ceil(100) as u32;
        for page in 1..=pages {
            let url = self.url(
                &["user", "repos"],
                &[
                    ("per_page", "100".to_owned()),
                    ("page", page.to_string()),
                    ("sort", "pushed".to_owned()),
                    ("direction", "desc".to_owned()),
                    (
                        "affiliation",
                        "owner,collaborator,organization_member".to_owned(),
                    ),
                ],
            );
            let (body, _) = self.get(url).await?;
            let found = items(&body);
            repos.extend(
                found
                    .iter()
                    .filter_map(|r| r.get("full_name").and_then(Value::as_str))
                    .filter_map(|name| RepoRef::parse(name).ok())
                    .filter(|r| owner.is_empty() || r.owner.eq_ignore_ascii_case(owner)),
            );
            if found.len() < 100 || repos.len() >= MAX_REPOS {
                break;
            }
        }
        repos.truncate(MAX_REPOS);
        Ok(repos)
    }

    async fn list_owners(&self) -> Result<Vec<String>, HostError> {
        let (me, _) = self.get(self.url(&["user"], &[])).await?;
        let login = me
            .get("login")
            .and_then(Value::as_str)
            .ok_or_else(|| HostError::Other("GitHub's reply for the token has no login".into()))?;
        let mut owners = vec![login.to_owned()];
        let (orgs, _) = self
            .get(self.url(&["user", "orgs"], &[("per_page", "100".to_owned())]))
            .await?;
        owners.extend(
            items(&orgs)
                .iter()
                .filter_map(|org| org.get("login").and_then(Value::as_str))
                .map(str::to_owned),
        );
        Ok(owners)
    }
}

fn transport(error: reqwest::Error) -> HostError {
    HostError::Other(format!("cannot reach GitHub: {}", error.without_url()))
}

/// The body of `response`, refused when it is over [`MAX_BODY_BYTES`].
async fn read_body(response: reqwest::Response) -> Result<Vec<u8>, HostError> {
    if response
        .content_length()
        .is_some_and(|len| len > MAX_BODY_BYTES as u64)
    {
        return Err(HostError::Other("GitHub's reply is too large".into()));
    }
    let bytes = response.bytes().await.map_err(transport)?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(HostError::Other("GitHub's reply is too large".into()));
    }
    Ok(bytes.to_vec())
}

/// The reason GitHub gave for a refusal, cut short.
fn reason(bytes: &[u8]) -> String {
    let message = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    message.chars().take(200).collect()
}

/// What a failed answer means to a caller.
fn classify(status: StatusCode, headers: &HeaderMap, bytes: &[u8]) -> HostError {
    classify_at(status, headers, bytes, Utc::now())
}

fn classify_at(
    status: StatusCode,
    headers: &HeaderMap,
    bytes: &[u8],
    now: DateTime<Utc>,
) -> HostError {
    let message = reason(bytes);
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    match status {
        StatusCode::UNAUTHORIZED => HostError::Unauthorized(message),
        StatusCode::NOT_FOUND | StatusCode::GONE => HostError::NotFound(message),
        StatusCode::UNPROCESSABLE_ENTITY => HostError::Rejected(
            // The reason that names the field, such as a head branch that is not on the host, is
            // in `errors`, not in the message.
            serde_json::from_slice::<Value>(bytes)
                .ok()
                .and_then(|v| {
                    v.get("errors")?
                        .as_array()?
                        .iter()
                        .filter_map(|e| e.get("message").and_then(Value::as_str))
                        .next()
                        .map(str::to_owned)
                })
                .map_or(message.clone(), |detail| format!("{message}: {detail}")),
        ),
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
            if let Some(seconds) = headers
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
            {
                return HostError::RateLimited {
                    retry_after: Duration::from_secs(seconds.max(1)),
                };
            }
            if header("x-ratelimit-remaining").is_some_and(|v| v.trim() == "0") {
                let reset = header("x-ratelimit-reset").and_then(|v| v.trim().parse::<i64>().ok());
                let wait = reset
                    .map(|at| (at - now.timestamp()).max(1) as u64)
                    .map_or(UNSTATED_LIMIT, Duration::from_secs);
                return HostError::RateLimited { retry_after: wait };
            }
            if status == StatusCode::TOO_MANY_REQUESTS
                || message.to_ascii_lowercase().contains("rate limit")
            {
                return HostError::RateLimited {
                    retry_after: UNSTATED_LIMIT,
                };
            }
            HostError::Unauthorized(if message.is_empty() {
                "the token may not do this".into()
            } else {
                message
            })
        }
        other => HostError::Other(format!(
            "GitHub answered {}{}",
            other.as_u16(),
            if message.is_empty() {
                String::new()
            } else {
                format!(": {message}")
            }
        )),
    }
}

fn items(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The issue in one item of a listing or a search, or `None` for a pull request. `repo` is the
/// repo asked about; a search names the repo in each item.
fn issue_from(item: &Value, repo: Option<&RepoRef>, etag: Option<String>) -> Option<Issue> {
    // GitHub lists pull requests among issues. A pull request has this key.
    if item.get("pull_request").is_some() {
        return None;
    }
    let repo = match repo {
        Some(repo) => repo.clone(),
        None => repo_of(item)?,
    };
    let state = match item.get("state").and_then(Value::as_str)? {
        "open" => IssueState::Open,
        "closed" => IssueState::Closed,
        _ => return None,
    };
    let names = |key: &str, field: &str| -> Vec<String> {
        item.get(key)
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|entry| match entry {
                        Value::String(name) => Some(name.clone()),
                        other => other.get(field).and_then(Value::as_str).map(str::to_owned),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(Issue {
        repo,
        number: item.get("number").and_then(Value::as_u64)?,
        title: text(item, "title"),
        body: text(item, "body"),
        labels: names("labels", "name"),
        assignees: names("assignees", "login"),
        author: item
            .get("user")
            .map(|user| text(user, "login"))
            .unwrap_or_default(),
        state,
        url: text(item, "html_url"),
        etag,
    })
}

/// The repo of a search hit, from its `repository_url` (`.../repos/{owner}/{name}`).
fn repo_of(item: &Value) -> Option<RepoRef> {
    let url = Url::parse(item.get("repository_url")?.as_str()?).ok()?;
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let [.., owner, name] = segments.as_slice() else {
        return None;
    };
    RepoRef::parse(&format!("{owner}/{name}")).ok()
}

/// The pull request in one item of a listing or the reply to opening one.
fn pull_from(item: &Value, repo: &RepoRef) -> Option<PullRequest> {
    let merged = item.get("merged_at").is_some_and(|at| !at.is_null());
    let state = match item.get("state").and_then(Value::as_str)? {
        "open" => "open",
        _ if merged => "merged",
        _ => "closed",
    };
    let branch = |side: &str| item.get(side).map(|b| text(b, "ref")).unwrap_or_default();
    Some(PullRequest {
        repo: repo.to_string(),
        number: item.get("number").and_then(Value::as_u64)?,
        url: text(item, "html_url"),
        head: branch("head"),
        base: branch("base"),
        state: state.to_owned(),
        draft: item.get("draft").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn comment_from(item: &Value) -> Option<Comment> {
    let body = text(item, "body");
    Some(Comment {
        id: item.get("id").and_then(Value::as_u64)?,
        author: item
            .get("user")
            .map(|user| text(user, "login"))
            .unwrap_or_default(),
        created_at: DateTime::parse_from_rfc3339(item.get("created_at")?.as_str()?)
            .ok()?
            .with_timezone(&Utc),
        from_hive: body.contains(MARKER_PREFIX),
        body,
    })
}

/// The marker line for `key`. Characters that could end the HTML comment are replaced.
fn marker(key: &str) -> String {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '#' | '/' | '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{MARKER_PREFIX}{safe}{MARKER_SUFFIX}")
}

fn existing_has(comment: &Comment, marker: &str) -> bool {
    comment.body.lines().any(|line| line.trim() == marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).unwrap()
    }

    #[test]
    fn a_spent_limit_waits_until_its_reset() {
        let h = headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1100"),
        ]);
        let error = classify_at(StatusCode::FORBIDDEN, &h, b"{}", at(1000));
        assert_eq!(
            error,
            HostError::RateLimited {
                retry_after: Duration::from_secs(100)
            }
        );
        // A reset already past still waits a second, not zero.
        let error = classify_at(StatusCode::FORBIDDEN, &h, b"{}", at(2000));
        assert_eq!(
            error,
            HostError::RateLimited {
                retry_after: Duration::from_secs(1)
            }
        );
    }

    #[test]
    fn a_secondary_limit_waits_what_retry_after_says() {
        let h = headers(&[("retry-after", "42")]);
        assert_eq!(
            classify_at(StatusCode::FORBIDDEN, &h, b"{}", at(0)),
            HostError::RateLimited {
                retry_after: Duration::from_secs(42)
            }
        );
        assert_eq!(
            classify_at(StatusCode::TOO_MANY_REQUESTS, &headers(&[]), b"{}", at(0)),
            HostError::RateLimited {
                retry_after: UNSTATED_LIMIT
            }
        );
        let body = br#"{"message":"You have exceeded a secondary rate limit."}"#;
        assert!(matches!(
            classify_at(StatusCode::FORBIDDEN, &headers(&[]), body, at(0)),
            HostError::RateLimited { .. }
        ));
    }

    #[test]
    fn a_refusal_that_is_not_a_limit_is_not_retried_as_one() {
        let body = br#"{"message":"Resource not accessible by personal access token"}"#;
        assert!(matches!(
            classify_at(StatusCode::FORBIDDEN, &headers(&[]), body, at(0)),
            HostError::Unauthorized(m) if m.contains("not accessible")
        ));
        assert!(matches!(
            classify_at(StatusCode::UNAUTHORIZED, &headers(&[]), b"{}", at(0)),
            HostError::Unauthorized(_)
        ));
        assert!(matches!(
            classify_at(StatusCode::NOT_FOUND, &headers(&[]), b"{}", at(0)),
            HostError::NotFound(_)
        ));
        assert!(matches!(
            classify_at(StatusCode::BAD_GATEWAY, &headers(&[]), b"", at(0)),
            HostError::Other(m) if m.contains("502")
        ));
    }

    #[test]
    fn a_pull_request_is_not_an_issue() {
        let repo = RepoRef::parse("example-org/api").unwrap();
        let pr = json!({"number": 5, "state": "open", "title": "x", "pull_request": {}});
        assert!(issue_from(&pr, Some(&repo), None).is_none());
        let issue = json!({
            "number": 6, "state": "open", "title": "Crash", "body": null,
            "labels": [{"name": "bug"}], "assignees": [{"login": "octo"}],
            "user": {"login": "reporter"}, "html_url": "https://github.example.org/x/6"
        });
        let read = issue_from(&issue, Some(&repo), Some("\"abc\"".into())).unwrap();
        assert_eq!(read.body, "");
        assert_eq!(read.labels, ["bug"]);
        assert_eq!(read.assignees, ["octo"]);
        assert_eq!(read.author, "reporter");
        assert_eq!(read.etag.as_deref(), Some("\"abc\""));
    }

    #[test]
    fn a_search_hit_names_its_repo_in_its_url() {
        let hit = json!({
            "number": 3, "state": "closed", "title": "t",
            "repository_url": "https://api.example.org/repos/example-org/web"
        });
        let issue = issue_from(&hit, None, None).unwrap();
        assert_eq!(issue.repo.to_string(), "example-org/web");
        assert_eq!(issue.state, IssueState::Closed);
    }

    #[test]
    fn a_marker_cannot_close_the_comment_it_lives_in() {
        let m = marker("comment:example-org/api#4:7 --> <script>");
        assert!(m.starts_with("<!-- buzz-hive:"));
        assert_eq!(m.matches("-->").count(), 1, "{m}");
        assert!(!m.contains('<') || m.starts_with("<!--"));
    }

    #[test]
    fn the_token_must_fit_a_header_and_the_address_must_be_a_base() {
        assert!(GitHub::new("https://api.example.org", "tok\nen").is_err());
        assert!(GitHub::new("not a url", "token").is_err());
        assert!(GitHub::new("ftp://api.example.org", "token").is_err());
        assert!(GitHub::new("https://api.example.org/?x=1", "token").is_err());
        assert!(GitHub::new("https://api.example.org/api/v3/", "token").is_ok());
    }

    #[test]
    fn a_repo_segment_cannot_climb_out_of_its_path() {
        let github = GitHub::new("https://api.example.org/api/v3", "token").unwrap();
        let url = github.url(&["repos", "a b", "c/d", "issues"], &[("q", "x&y=1".into())]);
        assert_eq!(
            url.as_str(),
            "https://api.example.org/api/v3/repos/a%20b/c%2Fd/issues?q=x%26y%3D1"
        );
    }
}
