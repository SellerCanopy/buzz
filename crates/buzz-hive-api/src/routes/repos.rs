//! The repos a card works in: a person adds and removes them on the card page.

use buzz_hive_core::domain::intake::RepoRef;
use buzz_hive_core::orchestrator::repos::{add_repo, list_repos, remove_repo};
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{AddIssueRepoRequest, Card, CardRepo, GetCardRequest, RemoveIssueRepoRequest};
use crate::routes::cards::get_card;
use crate::routes::intake::issue_ref;
use crate::routes::runs::parse_id;
use crate::unary::Ctx;

/// The repos of a card as the page shows them: empty for a card whose one repo is its issue's,
/// since the card's issue already names it.
pub(super) async fn card_repos(
    app: &App,
    ctx: &Ctx,
    card: Uuid,
    has_issue: bool,
) -> Result<Vec<CardRepo>, HiveError> {
    let repos = list_repos(app.store.as_ref(), &ctx.op(), card).await?;
    if repos.len() < 2 && (has_issue || repos.is_empty()) {
        return Ok(Vec::new());
    }
    Ok(repos
        .into_iter()
        .enumerate()
        .filter_map(|(at, repo)| {
            Some(CardRepo {
                repo: Some(issue_ref(&RepoRef::parse(&repo.repo).ok()?, 0)),
                main: repo.main,
                merge_order: i32::try_from(at + 1).unwrap_or(i32::MAX),
                active: true,
                remove_refusal: repo.remove_refusal,
                ..Default::default()
            })
        })
        .collect())
}

async fn reply(app: App, ctx: Ctx, card_id: String) -> Result<Card, HiveError> {
    get_card(app, ctx, GetCardRequest { card_id }).await
}

pub async fn add_issue_repo(
    app: App,
    ctx: Ctx,
    req: AddIssueRepoRequest,
) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let host = app.require_host()?;
    let (_, events) = add_repo(
        app.store.as_ref(),
        host,
        &ctx.op(),
        id,
        &req.repo,
        &req.base,
    )
    .await?;
    app.op(false)?.settle(&events);
    reply(app, ctx, req.card_id).await
}

pub async fn remove_issue_repo(
    app: App,
    ctx: Ctx,
    req: RemoveIssueRepoRequest,
) -> Result<Card, HiveError> {
    let id = parse_id(&req.card_id, "card_id")?;
    let (_, events) = remove_repo(app.store.as_ref(), &ctx.op(), id, &req.repo).await?;
    app.op(false)?.settle(&events);
    reply(app, ctx, req.card_id).await
}
