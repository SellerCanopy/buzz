//! Issues on a code host and the rules that bring them onto the board.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The longest rule name, in characters.
pub const MAX_RULE_NAME_CHARS: usize = 100;
/// The shortest interval a rule may set, in seconds.
pub const MIN_INTERVAL_S: u32 = 60;
/// The interval a rule gets when it names none, in seconds.
pub const DEFAULT_INTERVAL_S: u32 = 900;
/// The most authors a rule may list.
pub const MAX_AUTHORS: usize = 20;

/// One repo on the host, written `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    /// Reads `owner/name`. Both parts are letters, digits, `-`, `_` or `.`, as a host allows.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = || format!("{text:?} is not a repo written owner/name");
        let (owner, name) = text.split_once('/').ok_or_else(bad)?;
        let part_ok = |part: &str| {
            !part.is_empty()
                && part.len() <= 100
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        if !part_ok(owner) || !part_ok(name) {
            return Err(bad());
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    Open,
    Closed,
}

impl IssueState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// An issue as the host reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub repo: RepoRef,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    /// The login that opened the issue.
    pub author: String,
    pub state: IssueState,
    /// The issue's web page.
    pub url: String,
    /// The host's entity tag for the issue, when the read that returned it had one.
    pub etag: Option<String>,
}

/// What `list_issues` narrows a repo's issues by. Every field narrows the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueFilter {
    pub state: IssueState,
    pub label: Option<String>,
    /// Only issues no one is assigned.
    pub unassigned: bool,
}

impl Default for IssueFilter {
    fn default() -> Self {
        Self {
            state: IssueState::Open,
            label: None,
            unassigned: false,
        }
    }
}

/// One page of a listing. Pages count from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub number: u32,
    pub size: u32,
}

impl Page {
    pub fn first(size: u32) -> Self {
        Self { number: 1, size }
    }
}

pub type CommentId = u64;

/// A comment on an issue.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub id: CommentId,
    pub author: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// True for a comment Beehive wrote itself, which carries its marker.
    pub from_hive: bool,
}

/// What a rule keeps of an issue, besides the repo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleFilter {
    /// Only issues with this label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Only issues no one is assigned.
    #[serde(default)]
    pub unassigned: bool,
    /// When not empty, only issues these logins opened. These logins are also the only ones
    /// whose comments reach an agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
}

impl RuleFilter {
    /// The listing filter the host can apply itself.
    pub fn listing(&self) -> IssueFilter {
        IssueFilter {
            state: IssueState::Open,
            label: self.label.clone(),
            unassigned: self.unassigned,
        }
    }

    /// Whether `issue` passes the parts of the filter the host cannot apply.
    pub fn takes(&self, issue: &Issue) -> bool {
        self.authors.is_empty()
            || self
                .authors
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&issue.author))
    }

    /// Whether a comment by `login` may reach an agent.
    pub fn trusts(&self, login: &str) -> bool {
        self.authors.iter().any(|a| a.eq_ignore_ascii_case(login))
    }
}

/// A search that sends each new issue of one repo to a workflow, or to Backlog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeRule {
    pub id: Uuid,
    pub name: String,
    pub repo: RepoRef,
    pub filter: RuleFilter,
    /// The workflow the cards land on; empty fills Backlog.
    pub workflow: String,
    pub enabled: bool,
    pub interval_s: u32,
}

/// Why a card's issue is no longer in its rule's search.
pub const LEFT_CLOSED: &str = "closed";
pub const LEFT_UNMATCHED: &str = "unmatched";

/// An issue Beehive has read, and what it did with it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredIssue {
    pub issue: Issue,
    /// The card made from the issue. The card may since have been removed.
    pub card_id: Option<Uuid>,
    /// The rule that brought the card, `None` for a card a person made from the issue.
    pub rule_id: Option<Uuid>,
    /// Why the issue left its rule's search; empty while it is in it.
    pub left: String,
    /// Comments up to this time have been read.
    pub last_comment_at: Option<DateTime<Utc>>,
}

/// The query the issue search runs for `repos`, `text`, `label`, `assignee` and `state`,
/// in the host's own syntax. Pull requests are left out.
pub fn search_query(
    repos: &[RepoRef],
    text: &str,
    label: &str,
    assignee: &str,
    state: Option<IssueState>,
) -> String {
    let mut parts = vec!["is:issue".to_owned()];
    for repo in repos {
        parts.push(format!("repo:{repo}"));
    }
    if let Some(state) = state {
        parts.push(format!("state:{}", state.as_str()));
    }
    if !label.is_empty() {
        parts.push(format!("label:{}", quote(label)));
    }
    match assignee {
        "" => {}
        "none" => parts.push("no:assignee".into()),
        login => parts.push(format!("assignee:{}", quote(login))),
    }
    let text = text.trim();
    if !text.is_empty() {
        // Free text must not carry qualifiers of its own, or it could search another repo.
        let plain: Vec<String> = text
            .split_whitespace()
            .map(|word| word.replace(':', " "))
            .collect();
        parts.push(plain.join(" "));
    }
    parts.join(" ")
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_is_owner_slash_name() {
        let repo = RepoRef::parse("example-org/api").unwrap();
        assert_eq!(repo.to_string(), "example-org/api");
        for bad in ["", "api", "a/b/c", "/b", "a/", "a b/c", "../x", "a/.."] {
            assert!(RepoRef::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_search_query_names_the_repos_and_drops_pull_requests() {
        let repos = [
            RepoRef::parse("example-org/api").unwrap(),
            RepoRef::parse("example-org/web").unwrap(),
        ];
        assert_eq!(
            search_query(&repos, "crash on save", "bug", "none", Some(IssueState::Open)),
            "is:issue repo:example-org/api repo:example-org/web state:open label:\"bug\" no:assignee crash on save"
        );
    }

    #[test]
    fn free_text_cannot_add_a_qualifier() {
        let repos = [RepoRef::parse("example-org/api").unwrap()];
        let query = search_query(&repos, "org:other repo:x/y", "", "", None);
        assert!(!query.contains("org:other"), "{query}");
        assert!(!query.contains("repo:x/y"), "{query}");
    }

    #[test]
    fn only_listed_authors_are_trusted() {
        let filter = RuleFilter {
            authors: vec!["Octo".into()],
            ..RuleFilter::default()
        };
        assert!(filter.trusts("octo"));
        assert!(!filter.trusts("mallory"));
        assert!(!RuleFilter::default().trusts("octo"));
    }
}
