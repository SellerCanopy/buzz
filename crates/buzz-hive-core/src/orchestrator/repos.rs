//! The repos a card works in: its issue's repo, then the ones added to it.
//!
//! A card runs in its first repo, which is its issue's, or for a card with no issue the first
//! added. The others are linked context. The Go engine's per-repo worktrees, pull requests and
//! merge order are not here: see PORTING.md, Phase 21.

use serde_json::json;
use uuid::Uuid;

use super::intake::IntakeError;
use super::run::people_only;
use super::{Ctx, OpError};
use crate::domain::card::Card;
use crate::domain::events::Event;
use crate::domain::intake::{IssueFilter, Page, RepoRef};
use crate::ports::code_host::{CodeHost, HostError};
use crate::ports::store::{Store, StoreError, Tx};
use crate::workflow::names::valid_branch_name;

/// The most repos one card holds, its issue's included.
pub const MAX_CARD_REPOS: usize = 10;

/// One repo of a card, and why it cannot be removed now, if it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRepo {
    pub repo: String,
    /// The first repo, the one the card runs in.
    pub main: bool,
    /// Plain words for a person, or empty when nothing is in the way.
    pub remove_refusal: String,
}

/// The card's repos, first repo first: its issue's, then the added ones in the order added.
pub(crate) async fn repos_of(tx: &mut dyn Tx, card: Uuid) -> Result<Vec<String>, StoreError> {
    let mut repos: Vec<String> = tx
        .issues()
        .by_card(card)
        .await?
        .map(|row| row.issue.repo.to_string())
        .into_iter()
        .collect();
    for repo in tx.card_repos().of_card(card).await? {
        if !repos.contains(&repo) {
            repos.push(repo);
        }
    }
    Ok(repos)
}

/// The card's repos and, for each, what stops it being removed.
pub async fn list_repos(
    store: &dyn Store,
    ctx: &Ctx,
    card: Uuid,
) -> Result<Vec<CardRepo>, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    let repos = repos_of(&mut *tx, card).await?;
    let mut listed = Vec::with_capacity(repos.len());
    for repo in &repos {
        listed.push(CardRepo {
            repo: repo.clone(),
            main: repos.first() == Some(repo),
            remove_refusal: removal_refusal(&mut *tx, card, &repos, repo)
                .await?
                .unwrap_or_default(),
        });
    }
    Ok(listed)
}

/// Why `repo` cannot be taken off the card, or `None` when it can. A running stage is checked
/// by the caller, which holds the card's lock.
async fn removal_refusal(
    tx: &mut dyn Tx,
    card: Uuid,
    repos: &[String],
    repo: &str,
) -> Result<Option<String>, StoreError> {
    if tx
        .issues()
        .by_card(card)
        .await?
        .is_some_and(|row| row.issue.repo.to_string() == repo)
    {
        return Ok(Some("it is the repo of the card's issue".into()));
    }
    if repos.first().map(String::as_str) == Some(repo) && repos.len() > 1 {
        return Ok(Some(
            "it is the repo the card runs in; remove the others first".into(),
        ));
    }
    let open_pull = tx
        .pull_requests()
        .by_card(card)
        .await?
        .is_some_and(|pull| pull.repo == repo && pull.state == "open");
    Ok(open_pull.then(|| "it has an open pull request".into()))
}

/// Adds `repo` to the card, after checking the team's token reads it. `base` is checked as a
/// branch name and not kept: the card's branch is made from the repo's default branch.
pub async fn add_repo(
    store: &dyn Store,
    host: &dyn CodeHost,
    ctx: &Ctx,
    card: Uuid,
    repo: &str,
    base: &str,
) -> Result<(Card, Vec<Event>), IntakeError> {
    people_only(ctx, "add a repo to a card")?;
    let repo = RepoRef::parse(repo)
        .map_err(OpError::InvalidArgument)?
        .to_string();
    if !base.is_empty() && !valid_branch_name(base) {
        return Err(OpError::InvalidArgument(format!("base {base:?} is not a branch name")).into());
    }
    // Read before the host is asked, so a card that cannot take the repo costs no call.
    {
        let mut tx = store.begin(ctx.community).await?;
        check_can_add(&mut *tx, card, &repo).await?;
    }
    let repo_ref = RepoRef::parse(&repo).map_err(OpError::InvalidArgument)?;
    match host
        .list_issues(&repo_ref, &IssueFilter::default(), Page::first(1))
        .await
    {
        Err(HostError::NotFound(_)) => {
            return Err(
                OpError::NotFound(format!("{repo} does not exist or cannot be read")).into(),
            )
        }
        other => other?,
    };
    let mut tx = store.begin(ctx.community).await?;
    let found = check_can_add(&mut *tx, card, &repo).await?;
    tx.card_repos().add(card, &repo).await?;
    let repos = repos_of(&mut *tx, card).await?;
    tx.events()
        .append(Event::new(
            "card.repos",
            Some(card),
            None,
            ctx.actor.clone(),
            json!({ "repos": repos, "added": repo }),
        ))
        .await?;
    let events = tx.commit().await?;
    Ok((found, events))
}

/// `NotFound` for a card that is gone, `FailedPrecondition` while it runs, `AlreadyExists` for
/// a repo it has, and `FailedPrecondition` for one more than a card holds.
async fn check_can_add(tx: &mut dyn Tx, card: Uuid, repo: &str) -> Result<Card, OpError> {
    let found = tx
        .cards()
        .get_for_update(card)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card} does not exist")))?;
    check_frozen(tx, card).await?;
    let repos = repos_of(tx, card).await?;
    if repos.iter().any(|have| have == repo) {
        return Err(OpError::AlreadyExists(format!(
            "{repo} is already on the card"
        )));
    }
    if repos.len() >= MAX_CARD_REPOS {
        return Err(OpError::FailedPrecondition(format!(
            "a card holds at most {MAX_CARD_REPOS} repos"
        )));
    }
    Ok(found)
}

/// The repos are frozen while a run is active, so a stage does not find its repos changed.
async fn check_frozen(tx: &mut dyn Tx, card: Uuid) -> Result<(), OpError> {
    if tx.runs().active_for_card(card).await?.is_some() {
        return Err(OpError::FailedPrecondition(
            "the card has an active run; its repos change when the run ends".into(),
        ));
    }
    Ok(())
}

/// Takes `repo` off the card. The card's issue's repo, the repo the card runs in while others
/// remain, and a repo with an open pull request stay. `NotFound` for a repo the card lacks.
pub async fn remove_repo(
    store: &dyn Store,
    ctx: &Ctx,
    card: Uuid,
    repo: &str,
) -> Result<(Card, Vec<Event>), OpError> {
    people_only(ctx, "remove a repo from a card")?;
    let repo = RepoRef::parse(repo)
        .map_err(OpError::InvalidArgument)?
        .to_string();
    let mut tx = store.begin(ctx.community).await?;
    let found = tx
        .cards()
        .get_for_update(card)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card} does not exist")))?;
    check_frozen(&mut *tx, card).await?;
    let repos = repos_of(&mut *tx, card).await?;
    if !repos.contains(&repo) {
        return Err(OpError::NotFound(format!("{repo} is not on the card")));
    }
    if let Some(why) = removal_refusal(&mut *tx, card, &repos, &repo).await? {
        return Err(OpError::FailedPrecondition(format!(
            "{repo} cannot be removed: {why}"
        )));
    }
    tx.card_repos().remove(card, &repo).await?;
    let left = repos_of(&mut *tx, card).await?;
    tx.events()
        .append(Event::new(
            "card.repos",
            Some(card),
            None,
            ctx.actor.clone(),
            json!({ "repos": left, "removed": repo }),
        ))
        .await?;
    let events = tx.commit().await?;
    Ok((found, events))
}
