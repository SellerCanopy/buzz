//! Intake procedures: the rules that bring GitHub issues onto the board, the pickers that
//! browse the host, and the issue behind a card.
//!
//! One team token reads the host. Every procedure here needs the service to hold it, and
//! fails with `FailedPrecondition` naming the setting when it does not.

use buzz_hive_core::access::ActorKind;
use buzz_hive_core::background::intake::{refresh_rule, Outcome};
use buzz_hive_core::domain::intake::{IntakeRule, RepoRef, RuleFilter, StoredIssue};
use buzz_hive_core::orchestrator::intake::{
    delete_rule, list_rules, preview, save_rule, search, Browse,
};
use buzz_hive_core::ports::code_host::MAX_REPOS;
use chrono::Utc;
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    Card, DeleteIntakeRuleRequest, DeleteIntakeRuleResponse, Intake, IntakeRule as ProtoRule,
    IssueComment, IssueHit, IssueRef, ListIntakeRulesRequest, ListIntakeRulesResponse,
    ListIssueCommentsRequest, ListIssueCommentsResponse, ListOwnersRequest, ListOwnersResponse,
    ListReposRequest, ListReposResponse, PreviewIntakeRequest, PreviewIntakeResponse, PreviewIssue,
    RefreshIntakeRequest, RefreshIntakeResponse, SaveIntakeRuleRequest, SearchIssuesRequest,
    SearchIssuesResponse,
};
use crate::routes::poll::timestamp;
use crate::unary::Ctx;

/// The one host this service reads.
pub const HOST: &str = "github.com";
/// The most comments the Issue tab is sent.
const MAX_COMMENTS: usize = 100;

/// Refuses a host the service does not read. An empty host means [`HOST`].
fn check_host(host: &str) -> Result<(), HiveError> {
    if host.is_empty() || host.eq_ignore_ascii_case(HOST) {
        return Ok(());
    }
    Err(HiveError::InvalidArgument(format!(
        "host {host:?} is not supported; the only host is {HOST}"
    )))
}

fn parse_uuid(value: &str, field: &str) -> Result<Uuid, HiveError> {
    Uuid::parse_str(value).map_err(|_| HiveError::InvalidArgument(format!("{field} is not an id")))
}

/// A rule decides which outside text becomes cards and which comments reach agents, so only a
/// person changes one.
fn people_only(ctx: &Ctx) -> Result<(), HiveError> {
    if ctx.actor.kind == ActorKind::Agent {
        return Err(HiveError::PermissionDenied(
            "an agent may not change intake rules; ask a person".into(),
        ));
    }
    Ok(())
}

/// The rule a person wrote, as the engine keeps it. A rule names one repo and the filters a
/// host can apply itself; a typed query and "assigned to me" have no meaning for one team token.
fn rule_from_proto(rule: ProtoRule) -> Result<IntakeRule, HiveError> {
    let id = if rule.id.is_empty() {
        Uuid::nil()
    } else {
        parse_uuid(&rule.id, "id")?
    };
    let intake = rule
        .intake
        .ok_or_else(|| HiveError::InvalidArgument("intake is required".into()))?;
    intake_to_rule(id, rule.name, intake, rule.workflow, rule.enabled)
}

fn intake_to_rule(
    id: Uuid,
    name: String,
    intake: Intake,
    workflow: String,
    enabled: bool,
) -> Result<IntakeRule, HiveError> {
    check_host(&intake.host)?;
    if !intake.query.trim().is_empty() {
        return Err(HiveError::InvalidArgument(
            "a typed query is not supported; name a repo, and a label if you want one".into(),
        ));
    }
    if intake.assigned_to_me {
        return Err(HiveError::InvalidArgument(
            "assigned_to_me is not supported: the team token is not a person".into(),
        ));
    }
    let [repo] = intake.repos.as_slice() else {
        return Err(HiveError::InvalidArgument(
            "a rule names exactly one repo, written owner/name".into(),
        ));
    };
    let repo = RepoRef::parse(repo).map_err(HiveError::InvalidArgument)?;
    let unassigned = match intake.assignees.as_str() {
        "" | "anyone" => false,
        "unassigned" => true,
        other => {
            return Err(HiveError::InvalidArgument(format!(
                "assignees {other:?} is not supported; use anyone or unassigned"
            )))
        }
    };
    let interval_s = u32::try_from(intake.interval_s)
        .map_err(|_| HiveError::InvalidArgument("interval_s cannot be negative".into()))?;
    Ok(IntakeRule {
        id,
        name,
        repo,
        filter: RuleFilter {
            label: Some(intake.label).filter(|label| !label.is_empty()),
            unassigned,
            authors: intake.authors,
        },
        workflow,
        enabled,
        interval_s,
    })
}

fn rule_to_proto(rule: IntakeRule) -> ProtoRule {
    ProtoRule {
        id: rule.id.to_string(),
        name: rule.name,
        intake: Some(Intake {
            host: HOST.to_owned(),
            query: String::new(),
            interval_s: i32::try_from(rule.interval_s).unwrap_or(i32::MAX),
            assignees: if rule.filter.unassigned {
                "unassigned"
            } else {
                "anyone"
            }
            .to_owned(),
            authors: rule.filter.authors,
            repos: vec![rule.repo.to_string()],
            label: rule.filter.label.unwrap_or_default(),
            assigned_to_me: false,
        }),
        workflow: rule.workflow,
        enabled: rule.enabled,
    }
}

pub(super) fn issue_ref(repo: &RepoRef, number: u64) -> IssueRef {
    IssueRef {
        host: HOST.to_owned(),
        owner: repo.owner.clone(),
        repo: repo.name.clone(),
        number: i32::try_from(number).unwrap_or(i32::MAX),
    }
}

/// Sets the fields of `card` that come from the issue it was made from.
pub(super) fn fill_issue(card: &mut Card, row: &StoredIssue) {
    card.issue = Some(issue_ref(&row.issue.repo, row.issue.number));
    card.issue_url = row.issue.url.clone();
    card.intake_status = row.left.clone();
    card.intake_rule_id = row.rule_id.map(|id| id.to_string()).unwrap_or_default();
}

pub async fn save_intake_rule(
    app: App,
    ctx: Ctx,
    req: SaveIntakeRuleRequest,
) -> Result<ProtoRule, HiveError> {
    people_only(&ctx)?;
    let rule = rule_from_proto(
        req.rule
            .ok_or_else(|| HiveError::InvalidArgument("rule is required".into()))?,
    )?;
    let op = app.op(false)?;
    let saved = save_rule(&op.env(), ctx.community, rule).await?;
    app.intake.notify_one();
    Ok(rule_to_proto(saved))
}

pub async fn delete_intake_rule(
    app: App,
    ctx: Ctx,
    req: DeleteIntakeRuleRequest,
) -> Result<DeleteIntakeRuleResponse, HiveError> {
    people_only(&ctx)?;
    let id = parse_uuid(&req.id, "id")?;
    delete_rule(app.store.as_ref(), ctx.community, id).await?;
    app.intake.notify_one();
    Ok(DeleteIntakeRuleResponse {})
}

pub async fn list_intake_rules(
    app: App,
    ctx: Ctx,
    _req: ListIntakeRulesRequest,
) -> Result<ListIntakeRulesResponse, HiveError> {
    let (rules, seq) = list_rules(app.store.as_ref(), ctx.community).await?;
    Ok(ListIntakeRulesResponse {
        rules: rules.into_iter().map(rule_to_proto).collect(),
        seq,
    })
}

pub async fn preview_intake(
    app: App,
    _ctx: Ctx,
    req: PreviewIntakeRequest,
) -> Result<PreviewIntakeResponse, HiveError> {
    let host = app.require_host()?;
    let intake = req
        .rule
        .ok_or_else(|| HiveError::InvalidArgument("rule is required".into()))?;
    let rule = intake_to_rule(Uuid::nil(), String::new(), intake, String::new(), true)?;
    let op = app.op(false)?;
    let (issues, truncated) = preview(&op.env(), host, rule).await?;
    Ok(PreviewIntakeResponse {
        issues: issues
            .into_iter()
            .map(|issue| PreviewIssue {
                r#ref: Some(issue_ref(&issue.repo, issue.number)),
                title: issue.title,
                key: String::new(),
                url: issue.url,
            })
            .collect(),
        truncated,
    })
}

pub async fn refresh_intake(
    app: App,
    ctx: Ctx,
    req: RefreshIntakeRequest,
) -> Result<RefreshIntakeResponse, HiveError> {
    let host = app.require_host()?;
    let op = app.op(false)?;
    let (rules, _) = list_rules(app.store.as_ref(), ctx.community).await?;
    let rules: Vec<IntakeRule> = if req.rule_id.is_empty() {
        rules.into_iter().filter(|rule| rule.enabled).collect()
    } else {
        let id = parse_uuid(&req.rule_id, "rule_id")?;
        let rule = rules
            .into_iter()
            .find(|rule| rule.id == id)
            .ok_or_else(|| HiveError::NotFound(format!("intake rule {id} does not exist")))?;
        vec![rule]
    };
    if rules.is_empty() {
        return Err(HiveError::FailedPrecondition(
            "there is no intake rule to refresh".into(),
        ));
    }
    let mut total = Outcome::default();
    for rule in rules {
        // A rate limit fails the call at once: the loops wait it out, a request cannot.
        let (outcome, events) =
            refresh_rule(&op.env(), host, ctx.community, rule.id, Utc::now()).await?;
        op.settle(&events);
        total.matched += outcome.matched;
        total.added.extend(outcome.added);
        total.left += outcome.left;
        total.truncated |= outcome.truncated;
    }
    Ok(RefreshIntakeResponse {
        matched: i32::try_from(total.matched).unwrap_or(i32::MAX),
        added_card_ids: total.added.iter().map(Uuid::to_string).collect(),
        removed: 0,
        marked: i32::try_from(total.left).unwrap_or(i32::MAX),
        truncated: total.truncated,
    })
}

pub async fn list_repos(
    app: App,
    _ctx: Ctx,
    req: ListReposRequest,
) -> Result<ListReposResponse, HiveError> {
    check_host(&req.host)?;
    if req.owner.len() > 39
        || !req
            .owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '[' | ']'))
    {
        return Err(HiveError::InvalidArgument("owner is not a login".into()));
    }
    let repos = app.require_host()?.list_repos(&req.owner).await?;
    Ok(ListReposResponse {
        truncated: repos.len() >= MAX_REPOS,
        repos: repos.iter().map(RepoRef::to_string).collect(),
    })
}

pub async fn list_owners(
    app: App,
    _ctx: Ctx,
    req: ListOwnersRequest,
) -> Result<ListOwnersResponse, HiveError> {
    check_host(&req.host)?;
    Ok(ListOwnersResponse {
        owners: app.require_host()?.list_owners().await?,
    })
}

pub async fn search_issues(
    app: App,
    _ctx: Ctx,
    req: SearchIssuesRequest,
) -> Result<SearchIssuesResponse, HiveError> {
    check_host(&req.host)?;
    let host = app.require_host()?;
    let filter = req.filter.unwrap_or_default();
    let browse = Browse {
        repos: filter.repos,
        text: filter.text,
        label: filter.label,
        assignee: filter.assignee,
        state: filter.state,
    };
    let (issues, next_cursor, truncated) = search(host, &browse, &req.cursor).await?;
    Ok(SearchIssuesResponse {
        issues: issues
            .into_iter()
            .map(|issue| IssueHit {
                r#ref: Some(issue_ref(&issue.repo, issue.number)),
                title: issue.title,
                state: issue.state.as_str().to_owned(),
                labels: issue.labels,
                assignees: issue.assignees,
            })
            .collect(),
        next_cursor,
        truncated,
    })
}

/// The comments on a card's issue, read from the host now.
pub async fn list_issue_comments(
    app: App,
    ctx: Ctx,
    req: ListIssueCommentsRequest,
) -> Result<ListIssueCommentsResponse, HiveError> {
    let card = parse_uuid(&req.card_id, "card_id")?;
    let row = {
        let mut tx = app.store.begin(ctx.community).await?;
        tx.issues().by_card(card).await?
    }
    .ok_or_else(|| HiveError::NotFound(format!("card {card} has no issue")))?;
    let mut comments = app
        .require_host()?
        .list_comments(&row.issue.repo, row.issue.number, None)
        .await?;
    let truncated = comments.len() > MAX_COMMENTS;
    if truncated {
        comments.drain(..comments.len() - MAX_COMMENTS);
    }
    Ok(ListIssueCommentsResponse {
        comments: comments
            .into_iter()
            .map(|c| IssueComment {
                id: c.id.to_string(),
                author: c.author,
                body: c.body,
                at: Some(timestamp(c.created_at)),
                from_hive: c.from_hive,
            })
            .collect(),
        truncated,
    })
}

/// Sets the issue fields of every card in `cards` that has an issue, from one read.
pub(super) async fn fill_issues(app: &App, ctx: &Ctx, cards: &mut [Card]) -> Result<(), HiveError> {
    let rows = {
        let mut tx = app.store.begin(ctx.community).await?;
        tx.issues().attached().await?
    };
    for card in cards {
        let Ok(id) = Uuid::parse_str(&card.id) else {
            continue;
        };
        if let Some(row) = rows.iter().find(|row| row.card_id == Some(id)) {
            fill_issue(card, row);
        }
    }
    Ok(())
}

/// Sets the issue fields of `card`, when it was made from an issue. Returns the issue row.
pub(super) async fn fill_one(
    app: &App,
    ctx: &Ctx,
    card: &mut Card,
) -> Result<Option<StoredIssue>, HiveError> {
    let Ok(id) = Uuid::parse_str(&card.id) else {
        return Ok(None);
    };
    let row = {
        let mut tx = app.store.begin(ctx.community).await?;
        tx.issues().by_card(id).await?
    };
    if let Some(row) = &row {
        fill_issue(card, row);
    }
    Ok(row)
}
