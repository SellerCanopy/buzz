//! A card's artifacts: the documents and files a stage hands to the next one.
//!
//! Ported from the Go engine's `core/orchestrator/artifacts.go` and `scratch.go`. A save
//! appends a version and never rewrites one. Only the agent of the stage that is running may
//! save, and only an artifact the stage declares in `outputs` or names as its repeat plan. A
//! file is not stored here: its bytes live in the Buzz media store, and a version keeps the
//! hash, size and type of the blob and where to fetch it.

use serde_json::json;
use uuid::Uuid;

use super::routes::append;
use super::run::{check_agent, locked, running_stage};
use super::task_message::{InputState, InputView, INLINE_LIMIT};
use super::{Ctx, Env, OpError};
use crate::domain::artifact::{
    check_name, check_sha256, ArtifactContent, ArtifactInfo, ArtifactVersion, MAX_ARTIFACT_BYTES,
};
use crate::domain::events::Event;
use crate::domain::workflow::Stage;
use crate::ports::store::{ArtifactSave, StoreError, Tx};
use crate::rules::checkboxes::{count_checkboxes, set_checkbox};

/// The longest item `update_plan` takes.
pub const MAX_PLAN_ITEM_BYTES: usize = 1 << 10;

/// The longest media type a file version names.
const MAX_MEDIA_TYPE_CHARS: usize = 255;

/// The artifact names a stage owns: its declared outputs and its repeat plan.
pub fn own_artifacts(stage: &Stage) -> Vec<&str> {
    let mut names: Vec<&str> = stage.outputs.iter().map(|o| o.artifact.as_str()).collect();
    if let Some(plan) = stage.repeat.as_ref().map(|r| r.plan.as_str()) {
        if !plan.is_empty() && !names.contains(&plan) {
            names.push(plan);
        }
    }
    names
}

/// `Err` with a sentence when `content` is not something a version can hold.
fn check_content(content: &ArtifactContent) -> Result<(), OpError> {
    match content {
        ArtifactContent::Text(body) => {
            if body.len() > MAX_ARTIFACT_BYTES {
                return Err(OpError::InvalidArgument(format!(
                    "content is {} bytes; the artifact limit is {MAX_ARTIFACT_BYTES}",
                    body.len()
                )));
            }
        }
        ArtifactContent::File {
            sha256,
            size,
            media_type,
            media_ref,
        } => {
            check_sha256(sha256).map_err(OpError::InvalidArgument)?;
            if *size < 0 {
                return Err(OpError::InvalidArgument("size must not be negative".into()));
            }
            if media_type.is_empty() || media_type.chars().count() > MAX_MEDIA_TYPE_CHARS {
                return Err(OpError::InvalidArgument(format!(
                    "type is required and at most {MAX_MEDIA_TYPE_CHARS} characters"
                )));
            }
            if media_ref.is_empty() {
                return Err(OpError::InvalidArgument(
                    "a file needs its hash or url".into(),
                ));
            }
        }
    }
    Ok(())
}

fn saved_payload(version: &ArtifactVersion, extra: serde_json::Value) -> serde_json::Value {
    let mut payload = json!({
        "name": version.name,
        "version": version.version,
        "kind": version.content.kind().as_str(),
        "stage": version.stage,
        "visit": version.visit,
        "by": version.created_by,
        "sha256": version.content.sha256_hex(),
    });
    payload["bytes"] = match &version.content {
        ArtifactContent::Text(body) => json!(body.len()),
        ArtifactContent::File { size, .. } => json!(size),
    };
    if let (Some(fields), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        fields.extend(extra.clone());
    }
    payload
}

fn store_refusal(error: StoreError) -> OpError {
    match error {
        StoreError::Conflict(reason) => OpError::InvalidArgument(reason),
        other => other.into(),
    }
}

/// The running stage's agent saves the next version of one of the stage's artifacts.
pub async fn save_artifact(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    name: &str,
    content: ArtifactContent,
) -> Result<(ArtifactVersion, Vec<Event>), OpError> {
    check_name(name).map_err(OpError::InvalidArgument)?;
    check_content(&content)?;
    let mut tx = env.store.begin(ctx.community).await?;
    let (_, run, package) = locked(env, &mut *tx, card_id).await?;
    let stage_id = running_stage(&run)?;
    let (stage, _) = package.stage(&stage_id).ok_or_else(|| {
        OpError::FailedPrecondition(format!("workflow {} has no stage {stage_id}", run.workflow))
    })?;
    check_agent(&mut *tx, ctx, &run, &stage_id).await?;
    let own = own_artifacts(stage);
    if !own.contains(&name) {
        let declared = if own.is_empty() {
            "it declares no outputs".to_owned()
        } else {
            format!("it declares: {}", own.join(", "))
        };
        return Err(OpError::InvalidArgument(format!(
            "stage {stage_id} does not declare the artifact {name} in its outputs; {declared}"
        )));
    }
    let handoff = stage
        .outputs
        .iter()
        .find(|o| o.artifact == name)
        .and_then(|o| o.handoff)
        .unwrap_or(true);
    let version = tx
        .artifacts()
        .save(
            card_id,
            name,
            handoff,
            &ArtifactSave {
                content,
                stage: stage_id,
                visit: run.visit,
                created_by: ctx.actor.pubkey.clone(),
            },
        )
        .await
        .map_err(store_refusal)?;
    append(
        &mut *tx,
        ctx,
        "artifact.saved",
        &run,
        saved_payload(&version, json!({})),
    )
    .await?;
    let events = tx.commit().await?;
    Ok((version, events))
}

/// What `update_plan` did: the plan's counts after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanUpdate {
    pub plan: String,
    pub checked: u32,
    pub total: u32,
}

/// The running stage's agent checks or unchecks one item of its repeat plan, which saves a new
/// version of the plan. An item already in that state saves nothing.
pub async fn update_plan(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    item: &str,
    done: bool,
) -> Result<(PlanUpdate, Vec<Event>), OpError> {
    if item.trim().is_empty() {
        return Err(OpError::InvalidArgument("item is empty".into()));
    }
    if item.len() > MAX_PLAN_ITEM_BYTES {
        return Err(OpError::InvalidArgument(format!(
            "item is {} bytes; the limit is {MAX_PLAN_ITEM_BYTES}",
            item.len()
        )));
    }
    let mut tx = env.store.begin(ctx.community).await?;
    let (_, run, package) = locked(env, &mut *tx, card_id).await?;
    let stage_id = running_stage(&run)?;
    let (stage, _) = package.stage(&stage_id).ok_or_else(|| {
        OpError::FailedPrecondition(format!("workflow {} has no stage {stage_id}", run.workflow))
    })?;
    check_agent(&mut *tx, ctx, &run, &stage_id).await?;
    let plan = stage
        .repeat
        .as_ref()
        .map(|r| r.plan.clone())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            OpError::FailedPrecondition(format!("stage {stage_id} has no plan to update"))
        })?;
    let latest = tx.artifacts().version(card_id, &plan, None).await?;
    let Some(ArtifactVersion {
        content: ArtifactContent::Text(text),
        ..
    }) = latest
    else {
        return Err(OpError::FailedPrecondition(format!(
            "the plan {plan} has no saved version yet; save it with save_artifact first"
        )));
    };
    let next = set_checkbox(&text, item, done).map_err(OpError::InvalidArgument)?;
    let (checked, total) = count_checkboxes(&next);
    let update = PlanUpdate {
        plan: plan.clone(),
        checked,
        total,
    };
    if next == text {
        return Ok((update, Vec::new()));
    }
    let version = tx
        .artifacts()
        .save(
            card_id,
            &plan,
            true,
            &ArtifactSave {
                content: ArtifactContent::Text(next),
                stage: stage_id,
                visit: run.visit,
                created_by: ctx.actor.pubkey.clone(),
            },
        )
        .await
        .map_err(store_refusal)?;
    let extra = json!({ "item": item, "done": done, "checked": checked, "total": total });
    append(
        &mut *tx,
        ctx,
        "artifact.saved",
        &run,
        saved_payload(&version, extra),
    )
    .await?;
    let events = tx.commit().await?;
    Ok((update, events))
}

/// A person sets whether later stages that declare no inputs get an artifact.
pub async fn set_handoff(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    name: &str,
    handoff: bool,
) -> Result<(ArtifactInfo, Vec<Event>), OpError> {
    check_name(name).map_err(OpError::InvalidArgument)?;
    let mut tx = env.store.begin(ctx.community).await?;
    tx.cards()
        .get_for_update(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    tx.artifacts()
        .set_handoff(card_id, name, handoff)
        .await
        .map_err(|e| match e {
            StoreError::NotFound(_) => {
                OpError::NotFound(format!("card {card_id} has no artifact {name}"))
            }
            other => other.into(),
        })?;
    let info = tx
        .artifacts()
        .list(card_id)
        .await?
        .into_iter()
        .find(|a| a.name == name)
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} has no artifact {name}")))?;
    let event = Event::new(
        "artifact.handoff",
        Some(card_id),
        None,
        ctx.actor.clone(),
        json!({ "name": name, "handoff": handoff }),
    );
    tx.events().append(event).await?;
    let events = tx.commit().await?;
    Ok((info, events))
}

/// The card's artifacts, by name.
pub async fn list_artifacts(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<Vec<ArtifactInfo>, OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    Ok(tx.artifacts().list(card_id).await?)
}

/// Every version of one of the card's artifacts, oldest first.
pub async fn list_versions(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    name: &str,
) -> Result<Vec<ArtifactVersion>, OpError> {
    check_name(name).map_err(OpError::InvalidArgument)?;
    let mut tx = env.store.begin(ctx.community).await?;
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let versions = tx.artifacts().versions(card_id, name).await?;
    if versions.is_empty() {
        return Err(OpError::NotFound(format!(
            "card {card_id} has no artifact {name}"
        )));
    }
    Ok(versions)
}

/// One version of one of the card's artifacts, or the latest when `version` is 0.
pub async fn read_artifact(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
    name: &str,
    version: i32,
) -> Result<ArtifactVersion, OpError> {
    check_name(name).map_err(OpError::InvalidArgument)?;
    if version < 0 {
        return Err(OpError::InvalidArgument(
            "version must be 0 for the latest, or 1 or more".into(),
        ));
    }
    let mut tx = env.store.begin(ctx.community).await?;
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    tx.artifacts()
        .version(card_id, name, (version > 0).then_some(version))
        .await?
        .ok_or_else(|| {
            OpError::NotFound(format!(
                "card {card_id} has no artifact {name} at that version"
            ))
        })
}

/// The latest version of each artifact the card has, as the files it holds.
pub async fn list_files(
    env: &Env<'_>,
    ctx: &Ctx,
    card_id: Uuid,
) -> Result<Vec<ArtifactVersion>, OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    tx.cards()
        .get(card_id)
        .await?
        .ok_or_else(|| OpError::NotFound(format!("card {card_id} does not exist")))?;
    let mut files = Vec::new();
    for info in tx.artifacts().list(card_id).await? {
        if info.kind != crate::domain::artifact::ArtifactKind::File {
            continue;
        }
        if let Some(version) = tx.artifacts().version(card_id, &info.name, None).await? {
            files.push(version);
        }
    }
    Ok(files)
}

/// The names of the stage's required outputs that the stage's current visit has not saved.
pub(super) async fn missing_outputs(
    tx: &mut dyn Tx,
    card: Uuid,
    stage: &Stage,
    visit: u32,
) -> Result<Vec<String>, OpError> {
    if stage.outputs.iter().all(|o| o.optional) {
        return Ok(Vec::new());
    }
    let saved = tx.artifacts().saved_in(card, &stage.id, visit).await?;
    Ok(stage
        .outputs
        .iter()
        .filter(|o| !o.optional && !saved.contains(&o.artifact))
        .map(|o| o.artifact.clone())
        .collect())
}

/// The artifacts a stage's task lists: a stage that declares inputs gets those, in order, and
/// any of its own artifacts already saved. A stage that declares none gets every artifact
/// handed on, and its own.
pub(super) async fn task_inputs(
    tx: &mut dyn Tx,
    card: Uuid,
    stage: &Stage,
) -> Result<Vec<InputView>, OpError> {
    let infos = tx.artifacts().list(card).await?;
    let own = own_artifacts(stage);
    let mut names: Vec<(String, bool)> = Vec::new();
    if stage.inputs.is_empty() {
        for info in &infos {
            if info.handoff || own.contains(&info.name.as_str()) {
                names.push((info.name.clone(), false));
            }
        }
    } else {
        for input in &stage.inputs {
            names.push((input.name.clone(), true));
        }
        for info in &infos {
            if own.contains(&info.name.as_str()) && !names.iter().any(|(n, _)| *n == info.name) {
                names.push((info.name.clone(), false));
            }
        }
    }
    let mut views = Vec::new();
    for (name, declared) in names {
        match tx.artifacts().version(card, &name, None).await? {
            Some(v) => views.push(InputView {
                name,
                state: match v.content {
                    ArtifactContent::Text(body) => InputState::Text {
                        version: v.version,
                        bytes: body.len(),
                        body: (body.len() < INLINE_LIMIT).then_some(body),
                    },
                    ArtifactContent::File {
                        size,
                        media_type,
                        media_ref,
                        ..
                    } => InputState::File {
                        version: v.version,
                        size,
                        media_type,
                        media_ref,
                    },
                },
            }),
            None if declared => views.push(InputView {
                name,
                state: InputState::Missing,
            }),
            None => {}
        }
    }
    Ok(views)
}

/// The checked and total plan items of the stage's repeat plan, `(0, 0)` when it has none.
pub(super) async fn plan_progress(
    tx: &mut dyn Tx,
    card: Uuid,
    stage: &Stage,
) -> Result<(u32, u32), OpError> {
    let Some(plan) = stage
        .repeat
        .as_ref()
        .map(|r| r.plan.as_str())
        .filter(|p| !p.is_empty())
    else {
        return Ok((0, 0));
    };
    Ok(
        match tx
            .artifacts()
            .version(card, plan, None)
            .await?
            .map(|v| v.content)
        {
            Some(ArtifactContent::Text(text)) => count_checkboxes(&text),
            _ => (0, 0),
        },
    )
}
