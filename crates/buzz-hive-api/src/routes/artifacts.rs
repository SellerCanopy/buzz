//! Artifact and activity procedures: the documents and files stages hand to each other, the
//! plan a repeat stage ticks off, and the work and decisions an agent logs.

use buzz_hive_core::domain::artifact::{
    ArtifactContent, ArtifactInfo as DomainInfo, ArtifactVersion as DomainVersion,
};
use buzz_hive_core::orchestrator::artifacts::{
    list_artifacts as list, list_files as files, list_versions as versions, read_artifact as read,
    save_artifact as save, set_handoff as handoff, update_plan as plan,
};
use buzz_hive_core::orchestrator::{activity, Env};

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    ArtifactInfo, ArtifactVersion, CardFile, GetBriefRequest, GetBriefResponse,
    ListArtifactVersionsRequest, ListArtifactVersionsResponse, ListArtifactsRequest,
    ListArtifactsResponse, ListCardFilesRequest, ListCardFilesResponse, LogWorkRequest,
    LogWorkResponse, ReadArtifactRequest, ReadHistoryRequest, ReadHistoryResponse,
    RecordDecisionRequest, RecordDecisionResponse, SaveArtifactRequest, SetHandoffRequest,
    UpdatePlanRequest, UpdatePlanResponse,
};
use crate::routes::cards::to_activity;
use crate::routes::poll::timestamp;
use crate::routes::runs::parse_id;
use crate::unary::Ctx;

/// Runs `$body` with an `Env` named `$env`.
macro_rules! with_env {
    ($app:ident, $env:ident, $body:expr) => {{
        // Nothing here posts, so a service with no relay account still serves these.
        let hive = $app.hive_pubkey.clone();
        let catalog = $app.catalog.load();
        let $env = Env {
            store: $app.store.as_ref(),
            directory: $app.directory.as_ref(),
            catalog: &catalog,
            hive: &hive,
        };
        $body
    }};
}

fn info_to_proto(info: DomainInfo) -> ArtifactInfo {
    ArtifactInfo {
        name: info.name,
        latest: info.latest,
        handoff: info.handoff,
        kind: info.kind.as_str().to_owned(),
    }
}

/// A version as the wire shows it. `with_content` leaves a document's text out of a listing.
fn version_to_proto(version: DomainVersion, with_content: bool) -> ArtifactVersion {
    let sha256 = version.content.sha256_hex();
    let kind = version.content.kind().as_str().to_owned();
    let (content, size, media_type, media_ref) = match version.content {
        ArtifactContent::Text(body) => {
            let size = i64::try_from(body.len()).unwrap_or(i64::MAX);
            (
                if with_content { body } else { String::new() },
                size,
                String::new(),
                String::new(),
            )
        }
        ArtifactContent::File {
            size,
            media_type,
            media_ref,
            ..
        } => (String::new(), size, media_type, media_ref),
    };
    ArtifactVersion {
        name: version.name,
        version: version.version,
        content,
        sha256,
        by: version.created_by,
        created_at: Some(timestamp(version.created_at)),
        kind,
        size,
        media_type,
        media_ref,
        stage_id: version.stage,
        visit: i32::try_from(version.visit).unwrap_or(i32::MAX),
    }
}

pub async fn list_artifacts(
    app: App,
    ctx: Ctx,
    req: ListArtifactsRequest,
) -> Result<ListArtifactsResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let artifacts = with_env!(app, env, list(&env, &ctx.op(), card).await?);
    Ok(ListArtifactsResponse {
        artifacts: artifacts.into_iter().map(info_to_proto).collect(),
    })
}

pub async fn list_artifact_versions(
    app: App,
    ctx: Ctx,
    req: ListArtifactVersionsRequest,
) -> Result<ListArtifactVersionsResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let found = with_env!(app, env, versions(&env, &ctx.op(), card, &req.name).await?);
    Ok(ListArtifactVersionsResponse {
        versions: found
            .into_iter()
            .map(|v| version_to_proto(v, false))
            .collect(),
        approvals: Vec::new(),
    })
}

pub async fn read_artifact(
    app: App,
    ctx: Ctx,
    req: ReadArtifactRequest,
) -> Result<ArtifactVersion, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let found = with_env!(
        app,
        env,
        read(&env, &ctx.op(), card, &req.name, req.version).await?
    );
    Ok(version_to_proto(found, true))
}

pub async fn list_card_files(
    app: App,
    ctx: Ctx,
    req: ListCardFilesRequest,
) -> Result<ListCardFilesResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let found = with_env!(app, env, files(&env, &ctx.op(), card).await?);
    let files = found
        .into_iter()
        .filter_map(|v| {
            let ArtifactContent::File {
                sha256,
                size,
                media_type,
                media_ref,
            } = v.content
            else {
                return None;
            };
            Some(CardFile {
                name: v.name,
                size,
                url: media_ref,
                state: "saved".to_owned(),
                media_type,
                sha256,
                version: v.version,
                stage_id: v.stage,
                ..Default::default()
            })
        })
        .collect();
    Ok(ListCardFilesResponse { files })
}

pub async fn set_handoff(
    app: App,
    ctx: Ctx,
    req: SetHandoffRequest,
) -> Result<ArtifactInfo, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let (info, events) = with_env!(
        app,
        env,
        handoff(&env, &ctx.op(), card, &req.name, req.handoff).await?
    );
    app.bus.publish(&events);
    Ok(info_to_proto(info))
}

pub async fn save_artifact(
    app: App,
    ctx: Ctx,
    req: SaveArtifactRequest,
) -> Result<ArtifactVersion, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let content = match req.kind.as_str() {
        "" | "text" => ArtifactContent::Text(req.content),
        "file" => {
            if !req.content.is_empty() {
                return Err(HiveError::InvalidArgument(
                    "a file artifact has no content; send sha256, size, media_type and media_ref"
                        .into(),
                ));
            }
            ArtifactContent::File {
                sha256: req.sha256,
                size: req.size,
                media_type: req.media_type,
                media_ref: req.media_ref,
            }
        }
        other => {
            return Err(HiveError::InvalidArgument(format!(
                "kind is {other:?}; it must be text or file"
            )))
        }
    };
    let (version, events) = with_env!(
        app,
        env,
        save(&env, &ctx.op(), card, &req.name, content).await?
    );
    app.bus.publish(&events);
    Ok(version_to_proto(version, false))
}

pub async fn update_plan(
    app: App,
    ctx: Ctx,
    req: UpdatePlanRequest,
) -> Result<UpdatePlanResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let (update, events) = with_env!(
        app,
        env,
        plan(&env, &ctx.op(), card, &req.item, req.done).await?
    );
    app.bus.publish(&events);
    Ok(UpdatePlanResponse {
        plan: update.plan,
        checked: i32::try_from(update.checked).unwrap_or(i32::MAX),
        total: i32::try_from(update.total).unwrap_or(i32::MAX),
    })
}

pub async fn log_work(
    app: App,
    ctx: Ctx,
    req: LogWorkRequest,
) -> Result<LogWorkResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let event = with_env!(
        app,
        env,
        activity::log_work(
            &env,
            &ctx.op(),
            card,
            &req.summary,
            &req.detail,
            &req.plan_item
        )
        .await?
    );
    app.bus.publish(std::slice::from_ref(&event));
    Ok(LogWorkResponse {
        entry: Some(to_activity(event)),
    })
}

pub async fn record_decision(
    app: App,
    ctx: Ctx,
    req: RecordDecisionRequest,
) -> Result<RecordDecisionResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let rejected: Vec<_> = req
        .rejected
        .into_iter()
        .map(|r| activity::Rejected {
            option: r.option,
            reason: r.reason,
        })
        .collect();
    let event = with_env!(
        app,
        env,
        activity::record_decision(
            &env,
            &ctx.op(),
            card,
            &req.summary,
            &req.why,
            &rejected,
            &req.detail,
            req.supersedes,
        )
        .await?
    );
    app.bus.publish(std::slice::from_ref(&event));
    Ok(RecordDecisionResponse {
        entry: Some(to_activity(event)),
    })
}

pub async fn read_history(
    app: App,
    ctx: Ctx,
    req: ReadHistoryRequest,
) -> Result<ReadHistoryResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let entries = with_env!(
        app,
        env,
        activity::read_history(&env, &ctx.op(), card, req.after_seq, i64::from(req.limit)).await?
    );
    Ok(ReadHistoryResponse {
        entries: entries.into_iter().map(to_activity).collect(),
    })
}

pub async fn get_brief(
    app: App,
    ctx: Ctx,
    req: GetBriefRequest,
) -> Result<GetBriefResponse, HiveError> {
    let card = parse_id(&req.card_id, "card_id")?;
    let text = with_env!(
        app,
        env,
        activity::get_brief(&env, &ctx.op(), card, &req.stage_id).await?
    );
    Ok(GetBriefResponse { text })
}
