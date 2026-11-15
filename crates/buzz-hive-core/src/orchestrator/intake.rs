//! Intake rules, the issue search for the board's pickers, and cards made from an issue.
//!
//! The passes that read the host on a timer are in [`crate::background`].

use uuid::Uuid;

use super::cards::{insert_card, NewCard, MAX_DESCRIPTION_CHARS, MAX_TITLE_CHARS};
use super::{Ctx, Env, OpError};
use crate::domain::card::Card;
use crate::domain::events::Event;
use crate::domain::intake::{
    search_query, IntakeRule, Issue, IssueState, Page, RepoRef, DEFAULT_INTERVAL_S, MAX_AUTHORS,
    MAX_RULE_NAME_CHARS, MIN_INTERVAL_S,
};
use crate::ports::code_host::{CodeHost, HostError};
use crate::ports::store::{CommunityId, Store};

/// Issues read per page by intake and the preview.
pub const PAGE_SIZE: u32 = 100;
/// Issues per page of the browse search.
pub const SEARCH_PAGE_SIZE: u32 = 30;
/// The host returns at most this many results of one search.
pub const SEARCH_RESULT_CAP: u32 = 1000;
/// The most repos one search names.
pub const MAX_SEARCH_REPOS: usize = 5;

/// Why an operation that reads the host failed: the operation itself, or the host.
#[derive(Debug, thiserror::Error)]
pub enum IntakeError {
    #[error(transparent)]
    Op(#[from] OpError),
    #[error(transparent)]
    Host(#[from] HostError),
}

impl From<crate::ports::store::StoreError> for IntakeError {
    fn from(error: crate::ports::store::StoreError) -> Self {
        Self::Op(error.into())
    }
}

/// A card's title from an issue's: cut to the most a card holds.
pub fn card_title(issue: &Issue) -> String {
    let title = issue.title.trim();
    if title.is_empty() {
        return format!("{}#{}", issue.repo, issue.number);
    }
    title.chars().take(MAX_TITLE_CHARS).collect()
}

/// A card's description from an issue's body: cut to the most a card holds.
pub fn card_description(issue: &Issue) -> String {
    issue.body.chars().take(MAX_DESCRIPTION_CHARS).collect()
}

fn login_ok(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '[' | ']'))
}

/// A rule as a person wrote it, checked and cleaned. `id` is nil for a new rule.
pub fn check_rule(env: &Env<'_>, mut rule: IntakeRule) -> Result<IntakeRule, OpError> {
    rule.name = rule.name.trim().to_owned();
    if rule.name.chars().count() > MAX_RULE_NAME_CHARS {
        return Err(OpError::InvalidArgument(format!(
            "name must be at most {MAX_RULE_NAME_CHARS} characters"
        )));
    }
    if rule.interval_s == 0 {
        rule.interval_s = DEFAULT_INTERVAL_S;
    }
    if rule.interval_s < MIN_INTERVAL_S {
        return Err(OpError::InvalidArgument(format!(
            "interval_s must be at least {MIN_INTERVAL_S}"
        )));
    }
    rule.workflow = rule.workflow.trim().to_owned();
    if !rule.workflow.is_empty() && env.catalog.get(&rule.workflow).is_none() {
        return Err(OpError::NotFound(env.catalog.absent(&rule.workflow)));
    }
    let filter = &mut rule.filter;
    filter.label = filter
        .label
        .take()
        .map(|label| label.trim().to_owned())
        .filter(|label| !label.is_empty());
    if filter
        .label
        .as_ref()
        .is_some_and(|label| label.chars().count() > 100 || label.chars().any(char::is_control))
    {
        return Err(OpError::InvalidArgument(
            "label must be at most 100 characters with no control characters".into(),
        ));
    }
    let mut authors: Vec<String> = Vec::new();
    for author in &filter.authors {
        let author = author.trim().trim_start_matches('@');
        if author.is_empty() {
            continue;
        }
        if !login_ok(author) {
            return Err(OpError::InvalidArgument(format!(
                "{author:?} is not a login"
            )));
        }
        if !authors.iter().any(|a| a.eq_ignore_ascii_case(author)) {
            authors.push(author.to_owned());
        }
    }
    if authors.len() > MAX_AUTHORS {
        return Err(OpError::InvalidArgument(format!(
            "a rule lists at most {MAX_AUTHORS} authors"
        )));
    }
    filter.authors = authors;
    Ok(rule)
}

/// Saves a rule: adds it when its id is nil, else replaces the rule with that id.
pub async fn save_rule(
    env: &Env<'_>,
    community: CommunityId,
    rule: IntakeRule,
) -> Result<IntakeRule, OpError> {
    let mut rule = check_rule(env, rule)?;
    let mut tx = env.store.begin(community).await?;
    if rule.id.is_nil() {
        rule.id = Uuid::new_v4();
    } else if tx.intake().get(rule.id).await?.is_none() {
        return Err(OpError::NotFound(format!(
            "intake rule {} does not exist",
            rule.id
        )));
    }
    tx.intake().save(&rule).await?;
    tx.commit().await?;
    Ok(rule)
}

/// Deletes a rule. The cards it brought stay where they are.
pub async fn delete_rule(
    store: &dyn Store,
    community: CommunityId,
    id: Uuid,
) -> Result<(), OpError> {
    let mut tx = store.begin(community).await?;
    if !tx.intake().delete(id).await? {
        return Err(OpError::NotFound(format!(
            "intake rule {id} does not exist"
        )));
    }
    tx.commit().await?;
    Ok(())
}

/// Every rule, with the head event seq read in the same transaction.
pub async fn list_rules(
    store: &dyn Store,
    community: CommunityId,
) -> Result<(Vec<IntakeRule>, i64), OpError> {
    let mut tx = store.begin(community).await?;
    let seq = tx.events().latest_seq().await?;
    Ok((tx.intake().list().await?, seq))
}

/// The first page of issues a rule would take, and whether the repo has more. Nothing is
/// stored and no card is made.
pub async fn preview(
    env: &Env<'_>,
    host: &dyn CodeHost,
    rule: IntakeRule,
) -> Result<(Vec<Issue>, bool), IntakeError> {
    let rule = check_rule(env, rule)?;
    let page = host
        .list_issues(&rule.repo, &rule.filter.listing(), Page::first(PAGE_SIZE))
        .await?;
    let more = page.len() >= PAGE_SIZE as usize;
    let hits = page
        .into_iter()
        .filter(|issue| rule.filter.takes(issue))
        .collect();
    Ok((hits, more))
}

/// What the browse search narrows by. An empty field does not narrow.
#[derive(Debug, Clone, Default)]
pub struct Browse {
    pub repos: Vec<String>,
    pub text: String,
    pub label: String,
    /// `none`, or a login.
    pub assignee: String,
    /// `open`, `closed`, or empty for both.
    pub state: String,
}

/// One page of the browse search. `cursor` is the `next` of the page before, empty for the
/// first. Returns the issues, the cursor of the next page and whether the host cut the results.
pub async fn search(
    host: &dyn CodeHost,
    browse: &Browse,
    cursor: &str,
) -> Result<(Vec<Issue>, String, bool), IntakeError> {
    if browse.repos.is_empty() || browse.repos.len() > MAX_SEARCH_REPOS {
        return Err(
            OpError::InvalidArgument(format!("name one to {MAX_SEARCH_REPOS} repos")).into(),
        );
    }
    let repos = browse
        .repos
        .iter()
        .map(|repo| RepoRef::parse(repo).map_err(OpError::InvalidArgument))
        .collect::<Result<Vec<_>, _>>()?;
    let state = match browse.state.as_str() {
        "" => None,
        other => Some(
            IssueState::parse(other)
                .ok_or_else(|| OpError::InvalidArgument("state is open, closed or empty".into()))?,
        ),
    };
    if browse.assignee == "@me" {
        return Err(OpError::InvalidArgument(
            "the search runs with the team's token, so @me has no person to mean; name a login"
                .into(),
        )
        .into());
    }
    if !browse.assignee.is_empty() && browse.assignee != "none" && !login_ok(&browse.assignee) {
        return Err(OpError::InvalidArgument("assignee is none or a login".into()).into());
    }
    if browse.label.chars().count() > 100 || browse.label.chars().any(char::is_control) {
        return Err(OpError::InvalidArgument("label is not valid".into()).into());
    }
    if browse.text.chars().count() > 256 {
        return Err(OpError::InvalidArgument("text is over 256 characters".into()).into());
    }
    let number: u32 = if cursor.is_empty() {
        1
    } else {
        cursor
            .parse()
            .ok()
            .filter(|n| (1..=SEARCH_RESULT_CAP).contains(n))
            .ok_or_else(|| OpError::InvalidArgument("cursor is not one this search gave".into()))?
    };
    let query = search_query(&repos, &browse.text, &browse.label, &browse.assignee, state);
    let page = Page {
        number,
        size: SEARCH_PAGE_SIZE,
    };
    let found = host.search_issues(&query, page).await?;
    let full = found.len() >= SEARCH_PAGE_SIZE as usize;
    let reached = number * SEARCH_PAGE_SIZE;
    let next = if full && reached < SEARCH_RESULT_CAP {
        (number + 1).to_string()
    } else {
        String::new()
    };
    Ok((found, next, full && reached >= SEARCH_RESULT_CAP))
}

/// Makes a card from an issue, or returns the card the issue already has. The second value is
/// false when the card existed. A card removed since is made again, since a person asks.
pub async fn card_from_issue(
    env: &Env<'_>,
    ctx: &Ctx,
    host: &dyn CodeHost,
    repo: &RepoRef,
    number: u64,
    workflow: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(Card, bool, Vec<Event>), IntakeError> {
    let issue = host.get_issue(repo, number).await?;
    let mut tx = env.store.begin(ctx.community).await?;
    let row = tx.issues().upsert(&issue, now).await?;
    if let Some(existing) = match row.card_id {
        Some(id) => tx.cards().get(id).await?,
        None => None,
    } {
        return Ok((existing, false, Vec::new()));
    }
    let card = insert_card(
        &mut *tx,
        ctx,
        NewCard {
            title: card_title(&issue),
            description: card_description(&issue),
            workflow: workflow.to_owned(),
        },
    )
    .await?;
    tx.issues().attach(repo, number, card.id, None).await?;
    let events = tx.commit().await?;
    Ok((card, true, events))
}
