//! The actions that call the code host through the outbox (`open_pull_request`, `mark_ready` and
//! `merge`): what is asked of the host, and what the stage reports when the host has answered.
//!
//! The call is queued in the transaction that enters the stage (see `routes::apply`), like a
//! post, and the drain makes it. A call that is repeated after a failure finds the pull request
//! the first one opened, so a stage opens one pull request however often the call is made.

use super::routes::{apply, facts, resolve_settings, Mode};
use super::task_message::{base_values, render_template, CardText, IssueText};
use super::{Ctx, Env, OpError};
use crate::access::ActorKind;
use crate::domain::artifact::ArtifactContent;
use crate::domain::card::{card_branch, Card, PullRequest};
use crate::domain::events::{Actor, Event};
use crate::domain::run::{Run, RunState};
use crate::domain::workflow::{ActionArg, RESULT_FAILED, RESULT_PASSED};
use crate::ports::store::{CommunityId, OutboxRow, Posting, PullCall, Tx};
use crate::rules::run::{step, Input};
use crate::workflow::load::Package;
use crate::workflow::names::valid_branch_name;

/// The call that opens the pull request of `stage`, with its `with` arguments rendered:
/// `title` (the card's title when empty), `body` or the latest text of `body_artifact`, `draft`
/// (on unless it renders as `false`) and `base` (the repo's default branch when empty). The
/// body ends with a line that closes the card's issue when the pull request merges.
pub(super) async fn posting(
    env: &Env<'_>,
    tx: &mut dyn Tx,
    card: &Card,
    run: &Run,
    package: &Package,
    (stage, visit): (&str, u32),
    repo: &str,
) -> Result<Posting, OpError> {
    let (def, _) = package.stage(stage).ok_or_else(|| {
        OpError::FailedPrecondition(format!(
            "workflow {} has no stage {stage}",
            package.meta.name
        ))
    })?;
    let params = resolve_settings(env, tx, card, &run.parameters, Some(stage))
        .await?
        .param_values();
    let issue = tx.issues().by_card(card.id).await?.map(|row| IssueText {
        repo: row.issue.repo.to_string(),
        number: row.issue.number,
        url: row.issue.url,
    });
    let values = base_values(
        &CardText {
            id: card.id,
            title: &card.title,
            description: &card.description,
        },
        &card.description,
        &params,
        issue.as_ref(),
    );
    let render = |name: &str| -> String {
        let text = def
            .with
            .as_ref()
            .and_then(|with| with.get(name))
            .map_or("", ActionArg::text);
        render_template(text, &values).trim().to_owned()
    };
    let title = Some(render("title"))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| card.title.clone());
    let base = render("base");
    if !base.is_empty() && !valid_branch_name(&base) {
        return Err(OpError::FailedPrecondition(format!(
            "{stage}: base {base:?} is not a valid branch name"
        )));
    }
    let artifact = render("body_artifact");
    let mut body = if artifact.is_empty() {
        render("body")
    } else {
        match tx.artifacts().version(card.id, &artifact, None).await? {
            Some(version) => match version.content {
                ArtifactContent::Text(text) => text,
                ArtifactContent::File { .. } => {
                    return Err(OpError::FailedPrecondition(format!(
                        "{stage}: the artifact {artifact} is a file, and a pull request body is text"
                    )))
                }
            },
            None => {
                return Err(OpError::FailedPrecondition(format!(
                    "{stage}: the artifact {artifact} has no saved version to use as the pull request body"
                )))
            }
        }
    };
    if let Some(issue) = issue.filter(|i| i.repo == repo) {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!("Closes #{}", issue.number));
    }
    Ok(Posting::PullRequest {
        stage: stage.to_owned(),
        visit,
        repo: repo.to_owned(),
        head: card_branch(card.id, &card.title),
        base,
        title,
        body,
        draft: render("draft") != "false",
    })
}

/// What the stage reports once the call of `row`, a [`Posting::PullRequest`], is answered: the
/// pull request is recorded for the card and the stage passes, or the stage fails with the
/// reason. The row is marked done in the same transaction, so a crash between the host's answer
/// and here repeats the call, which finds the pull request again. A stage the run has left
/// reports nothing.
pub async fn finish(
    env: &Env<'_>,
    community: CommunityId,
    row: &OutboxRow,
    outcome: Result<PullRequest, String>,
) -> Result<Vec<Event>, OpError> {
    let (put, result, summary) = match outcome {
        Ok(pull) => {
            let summary = format!(
                "opened {}pull request #{} in {}: {}",
                if pull.draft { "draft " } else { "" },
                pull.number,
                pull.repo,
                pull.url
            );
            (Put::Pull(pull), RESULT_PASSED, summary)
        }
        Err(reason) => (Put::Nothing, RESULT_FAILED, reason),
    };
    settle(env, community, row, put, result, summary).await
}

/// What the stage reports once the call of `row`, a [`Posting::PullCall`], is answered. A call
/// that worked updates the stored pull request (ready, or merged) and passes the stage; one the
/// host refused fails it with the host's reason.
pub async fn finish_call(
    env: &Env<'_>,
    community: CommunityId,
    row: &OutboxRow,
    outcome: Result<(), String>,
) -> Result<Vec<Event>, OpError> {
    let Posting::PullCall {
        repo, number, call, ..
    } = &row.posting
    else {
        return Err(OpError::InvalidArgument(
            "the post is not a pull request call".into(),
        ));
    };
    let (put, result, summary) = match (outcome, call) {
        (Ok(()), PullCall::MarkReady) => (
            Put::Ready,
            RESULT_PASSED,
            format!("marked pull request #{number} in {repo} ready for review"),
        ),
        (Ok(()), PullCall::Merge { method }) => (
            Put::Merged,
            RESULT_PASSED,
            format!(
                "merged pull request #{number} in {repo} ({})",
                method.as_str()
            ),
        ),
        (Err(reason), _) => (Put::Nothing, RESULT_FAILED, reason),
    };
    settle(env, community, row, put, result, summary).await
}

/// What a finished call changes in the card's stored pull request.
enum Put {
    Nothing,
    /// The pull request the host opened or found.
    Pull(PullRequest),
    Ready,
    Merged,
}

async fn settle(
    env: &Env<'_>,
    community: CommunityId,
    row: &OutboxRow,
    put: Put,
    result: &str,
    summary: String,
) -> Result<Vec<Event>, OpError> {
    let Some((stage, visit)) = awaited(&row.posting) else {
        return Err(OpError::InvalidArgument(
            "the post is not a pull request call".into(),
        ));
    };
    let ctx = Ctx {
        community,
        actor: Actor {
            pubkey: env.hive.to_owned(),
            kind: ActorKind::Agent,
        },
        requester: None,
    };
    let mut tx = env.store.begin(community).await?;
    // The card first, then the run, as every other input takes them.
    let Some(mut card) = tx.cards().get_for_update(row.card_id).await? else {
        return done(tx, row.id).await;
    };
    let stored = match put {
        Put::Nothing => None,
        Put::Pull(pull) => Some(pull),
        Put::Ready => tx.pull_requests().by_card(card.id).await?.map(|mut pull| {
            pull.draft = false;
            pull
        }),
        Put::Merged => tx.pull_requests().by_card(card.id).await?.map(|mut pull| {
            pull.state = "merged".into();
            pull
        }),
    };
    if let Some(pull) = &stored {
        tx.pull_requests().put(card.id, pull).await?;
    }
    let Some(active) = tx.runs().active_for_card(card.id).await? else {
        return done(tx, row.id).await;
    };
    let Some(run) = tx.runs().get_for_update(active.id).await? else {
        return done(tx, row.id).await;
    };
    if !is_waiting(&run, stage, visit) {
        return done(tx, row.id).await;
    }
    let package = env
        .catalog
        .get(&run.workflow)
        .ok_or_else(|| OpError::FailedPrecondition(env.catalog.absent(&run.workflow)))?;
    let facts = facts(&mut *tx, &card, package, Some(stage)).await?;
    let stepped = step(
        &run,
        package,
        &facts,
        Input::StageDone {
            stage: stage.to_owned(),
            result: result.to_owned(),
            summary,
        },
    );
    apply(
        env,
        &ctx,
        &mut *tx,
        &mut card,
        package,
        stepped,
        Mode::Update,
    )
    .await?;
    done(tx, row.id).await
}

/// The stage and visit that wait for the call of a post, when it is a call to the code host.
fn awaited(posting: &Posting) -> Option<(&str, u32)> {
    match posting {
        Posting::PullRequest { stage, visit, .. } | Posting::PullCall { stage, visit, .. } => {
            Some((stage, *visit))
        }
        _ => None,
    }
}

fn is_waiting(run: &Run, stage: &str, visit: u32) -> bool {
    run.state == RunState::Running
        && run.current_stage.as_deref() == Some(stage)
        && run.visit == visit
}

/// Whether the run still waits for the call of `row`. A run that was stopped, or moved on, no
/// longer wants its pull request, and the call is dropped instead of made.
pub async fn wanted(
    env: &Env<'_>,
    community: CommunityId,
    row: &OutboxRow,
) -> Result<bool, OpError> {
    let Some((stage, visit)) = awaited(&row.posting) else {
        return Ok(false);
    };
    let mut tx = env.store.begin(community).await?;
    let run = tx.runs().active_for_card(row.card_id).await?;
    Ok(run.is_some_and(|run| is_waiting(&run, stage, visit)))
}

/// Marks the call done and commits.
async fn done(mut tx: Box<dyn Tx>, row: i64) -> Result<Vec<Event>, OpError> {
    match tx.outbox().mark_done(row).await {
        Ok(()) | Err(crate::ports::store::StoreError::NotFound(_)) => {}
        Err(other) => return Err(other.into()),
    }
    Ok(tx.commit().await?)
}
