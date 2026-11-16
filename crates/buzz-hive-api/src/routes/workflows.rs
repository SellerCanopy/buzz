//! Workflow procedures: list the packages, check them and offer what a draft may use.

use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read, Write};

use buzz_hive_core::access::{allow, Level};
use buzz_hive_core::domain::workflow::{Finding, Stage};
use buzz_hive_core::orchestrator::save::{
    check_draft, delete_workflow as delete, draft_files, draft_package as draft_of,
    duplicate_workflow as duplicate, edit_workflow as edit, propose_workflow, renamed,
    save_workflow as save, set_workflow_options as set_options, Saver, MAX_DRAFT_BYTES,
    MAX_DRAFT_FILES,
};
use buzz_hive_core::workflow::actions::{stage_needs_code_host, stage_needs_issue_host};
use buzz_hive_core::workflow::builtin::{builtin_templates, ENGINE_VERSION};
use buzz_hive_core::workflow::check::{check, has_errors, CheckEnv};
use buzz_hive_core::workflow::load::{Catalog, Package};
use buzz_hive_core::workflow::options::resolve_options;
use buzz_hive_core::workflow::options::WorkflowOptions;
use buzz_hive_core::workflow::saved::SAVED_LABEL;
use buzz_hive_core::workflow::template::{draft_variables, variables};
use buzz_hive_core::workflow::templates::load_stage_templates;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    CheckWorkflowRequest, CheckWorkflowResponse, DeleteWorkflowRequest, DeleteWorkflowResponse,
    DuplicateWorkflowRequest, EditWorkflowRequest, EditWorkflowResponse, ExportWorkflowRequest,
    ExportWorkflowResponse, GetWorkflowDraftRequest, GetWorkflowDraftResponse,
    ImportWorkflowRequest, ListStageTemplatesRequest, ListStageTemplatesResponse,
    ListTemplateVariablesRequest, ListTemplateVariablesResponse, ListWorkflowsRequest,
    ListWorkflowsResponse, PackageCheck, PackageParameter, SaveWorkflowRequest,
    SaveWorkflowResponse, SetWorkflowOptionsRequest, StageTemplate, TemplateVariable, Workflow,
    WorkflowDraft, WorkflowStage,
};
use crate::unary::Ctx;

pub async fn list_workflows(
    app: App,
    ctx: Ctx,
    _req: ListWorkflowsRequest,
) -> Result<ListWorkflowsResponse, HiveError> {
    let catalog = app.catalog.load();
    let options = stored_options(&app, &ctx).await?;
    let workflows = catalog
        .names()
        .into_iter()
        .map(|name| workflow_of(&catalog, name, &options))
        .collect();
    Ok(ListWorkflowsResponse { workflows })
}

async fn stored_options(
    app: &App,
    ctx: &Ctx,
) -> Result<BTreeMap<String, WorkflowOptions>, HiveError> {
    let mut tx = app.store.begin(ctx.community).await?;
    Ok(tx.workflows().options().await?)
}

/// The listed form of the loaded workflow `name`, which must be in `catalog`.
fn workflow_of(
    catalog: &Catalog,
    name: &str,
    options: &BTreeMap<String, WorkflowOptions>,
) -> Workflow {
    let env = CheckEnv {
        catalog: Some(catalog),
    };
    let packages = catalog.all(name);
    let first = &packages[0];
    let ok = packages.iter().all(|p| !has_errors(&check(p, &env)));
    let resolved = resolve_options(first, options.get(name));
    let stages = &first.workflow.stages;
    Workflow {
        name: name.to_owned(),
        version: if first.builtin {
            ENGINE_VERSION.to_owned()
        } else {
            first.meta.version.clone()
        },
        description: first.meta.description.clone(),
        paths: packages.iter().map(|p| p.path.clone()).collect(),
        ok,
        builtin: first.builtin,
        own: !first.builtin && first.path.starts_with(&format!("{SAVED_LABEL}/")),
        needs_issue_host: stages.iter().any(stage_needs_issue_host),
        needs_code_host: stages.iter().any(stage_needs_code_host),
        paused_stages: resolved.paused,
        next_workflow: resolved.next,
        labels: resolved.labels,
        stages: stages.iter().map(stage_to_proto).collect(),
    }
}

fn stage_to_proto(stage: &Stage) -> WorkflowStage {
    WorkflowStage {
        id: stage.id.clone(),
        name: if stage.name.is_empty() {
            stage.id.clone()
        } else {
            stage.name.clone()
        },
        kind: stage.kind.clone(),
        agent: stage.agent.clone().unwrap_or_default(),
        pause: stage.pause,
    }
}

pub async fn check_workflow(
    app: App,
    ctx: Ctx,
    req: CheckWorkflowRequest,
) -> Result<CheckWorkflowResponse, HiveError> {
    if let Some(draft) = &req.draft {
        let package = draft_package(draft)?;
        let op = app.op(false)?;
        let findings = check_draft(&op.env(), &ctx.op(), &package).await?;
        return Ok(response(vec![package_check(
            &package.meta.name,
            &package.path,
            &findings,
        )]));
    }
    let catalog = app.catalog.load();
    let env = CheckEnv {
        catalog: Some(&catalog),
    };
    let mut packages = Vec::new();
    if req.name.is_empty() {
        for name in catalog.names() {
            for package in catalog.all(name) {
                packages.push(package_check(name, &package.path, &check(package, &env)));
            }
        }
        // A folder that failed to load has no package name. Its finding is reported with
        // the folder as the path.
        for finding in &catalog.findings {
            let bare = Finding {
                path: String::new(),
                ..finding.clone()
            };
            packages.push(package_check("", &finding.path, &[bare]));
        }
    } else {
        let loaded = catalog.all(&req.name);
        if loaded.is_empty() {
            return Err(HiveError::NotFound(format!(
                "workflow {:?} is not loaded",
                req.name
            )));
        }
        for package in loaded {
            packages.push(package_check(
                &req.name,
                &package.path,
                &check(package, &env),
            ));
        }
    }
    Ok(response(packages))
}

fn response(packages: Vec<PackageCheck>) -> CheckWorkflowResponse {
    let ok = packages
        .iter()
        .all(|p| p.findings.iter().all(|f| f.severity != "error"));
    CheckWorkflowResponse {
        packages,
        ok,
        ..Default::default()
    }
}

fn package_check(name: &str, path: &str, findings: &[Finding]) -> PackageCheck {
    PackageCheck {
        name: name.to_owned(),
        path: path.to_owned(),
        findings: to_findings(findings),
    }
}

/// The package a draft holds. A bad file name, a file that is not text or a name that
/// differs from the one in `kiln-package.yaml` is `InvalidArgument`.
fn draft_package(draft: &WorkflowDraft) -> Result<Package, HiveError> {
    let files = draft
        .files
        .iter()
        .map(|(name, bytes)| (name.clone(), bytes.to_vec()))
        .collect();
    Ok(draft_of(&draft.name, files)?)
}

pub async fn list_template_variables(
    _app: App,
    _ctx: Ctx,
    req: ListTemplateVariablesRequest,
) -> Result<ListTemplateVariablesResponse, HiveError> {
    let list = match &req.draft {
        Some(draft) => draft_variables(&draft_package(draft)?),
        None => variables().into_iter().filter(|v| !v.pattern).collect(),
    };
    Ok(ListTemplateVariablesResponse {
        variables: list
            .into_iter()
            .map(|v| TemplateVariable {
                name: v.name,
                description: v.description,
            })
            .collect(),
    })
}

pub async fn list_stage_templates(
    _app: App,
    _ctx: Ctx,
    _req: ListStageTemplatesRequest,
) -> Result<ListStageTemplatesResponse, HiveError> {
    let templates = load_stage_templates(&builtin_templates()).map_err(HiveError::internal)?;
    Ok(ListStageTemplatesResponse {
        templates: templates
            .into_iter()
            .map(|t| StageTemplate {
                id: t.id,
                stage_json: t.json,
            })
            .collect(),
    })
}

/// Who a save is for: the person, or the person an agent acts for.
fn saver(ctx: &Ctx) -> Saver {
    Saver {
        pubkey: ctx
            .requester_pubkey
            .clone()
            .unwrap_or_else(|| ctx.actor.pubkey.clone()),
        admin: allow(ctx.actor.level, ctx.requester, Level::Admin).is_ok(),
    }
}

/// The listed form of workflow `name` after a change, read from a catalog that has it.
async fn listed(app: &App, ctx: &Ctx, name: &str) -> Result<Workflow, HiveError> {
    let catalog = app.catalog.load();
    if catalog.get(name).is_none() {
        return Err(HiveError::NotFound(catalog.absent(name)));
    }
    let options = stored_options(app, ctx).await?;
    Ok(workflow_of(&catalog, name, &options))
}

pub(super) fn to_findings(findings: &[Finding]) -> Vec<crate::proto::Finding> {
    findings
        .iter()
        .map(|f| crate::proto::Finding {
            path: f.path.clone(),
            code: f.code.clone(),
            severity: f.severity.as_str().to_owned(),
            message: f.message.clone(),
        })
        .collect()
}

pub async fn get_workflow_draft(
    app: App,
    _ctx: Ctx,
    req: GetWorkflowDraftRequest,
) -> Result<GetWorkflowDraftResponse, HiveError> {
    let catalog = app.catalog.load();
    let package = catalog
        .get(&req.name)
        .ok_or_else(|| HiveError::NotFound(catalog.absent(&req.name)))?;
    let files = draft_files(package)?;
    let (bytes, text) = if req.text {
        (HashMap::new(), files.into_iter().collect())
    } else {
        (
            files
                .into_iter()
                .map(|(name, text)| (name, text.into_bytes()))
                .collect(),
            HashMap::new(),
        )
    };
    Ok(GetWorkflowDraftResponse {
        draft: Some(WorkflowDraft {
            name: req.name,
            files: bytes,
        }),
        hash: package.hash(),
        parameters: package
            .meta
            .parameters
            .iter()
            .map(|p| PackageParameter {
                name: p.name.clone(),
                r#type: p.kind.map(|k| k.as_str().to_owned()).unwrap_or_default(),
                default_value: p.default.clone(),
                description: p.description.clone(),
                choices: p.choices.clone(),
            })
            .collect(),
        text_files: text,
        ..Default::default()
    })
}

pub async fn save_workflow(
    app: App,
    ctx: Ctx,
    req: SaveWorkflowRequest,
) -> Result<SaveWorkflowResponse, HiveError> {
    let draft = req
        .draft
        .as_ref()
        .ok_or_else(|| HiveError::InvalidArgument("the request has no draft".into()))?;
    let package = draft_package(draft)?;
    let name = package.meta.name.clone();
    let base = Some(req.base_hash.as_str()).filter(|h| !h.is_empty());
    let saved = save(
        &app.op(false)?.env(),
        &ctx.op(),
        &saver(&ctx),
        package,
        base,
    )
    .await?;
    app.refresh_saved().await?;
    Ok(SaveWorkflowResponse {
        workflow: Some(listed(&app, &ctx, &name).await?),
        hash: saved.hash,
    })
}

pub async fn duplicate_workflow(
    app: App,
    ctx: Ctx,
    req: DuplicateWorkflowRequest,
) -> Result<Workflow, HiveError> {
    let op = app.op(false)?;
    duplicate(&op.env(), &ctx.op(), &saver(&ctx), &req.name, &req.new_name).await?;
    app.refresh_saved().await?;
    listed(&app, &ctx, &req.new_name).await
}

pub async fn delete_workflow(
    app: App,
    ctx: Ctx,
    req: DeleteWorkflowRequest,
) -> Result<DeleteWorkflowResponse, HiveError> {
    delete(&app.op(false)?.env(), &ctx.op(), &req.name).await?;
    app.refresh_saved().await?;
    Ok(DeleteWorkflowResponse {})
}

pub async fn set_workflow_options(
    app: App,
    ctx: Ctx,
    req: SetWorkflowOptionsRequest,
) -> Result<Workflow, HiveError> {
    let options = WorkflowOptions {
        paused_stages: Some(req.paused_stages),
        next_workflow: req.next_workflow,
        labels: req
            .labels
            .into_iter()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect(),
    };
    set_options(&app.op(false)?.env(), &ctx.op(), &req.workflow, options).await?;
    listed(&app, &ctx, &req.workflow).await
}

pub async fn export_workflow(
    app: App,
    _ctx: Ctx,
    req: ExportWorkflowRequest,
) -> Result<ExportWorkflowResponse, HiveError> {
    let catalog = app.catalog.load();
    let package = catalog
        .get(&req.name)
        .ok_or_else(|| HiveError::NotFound(catalog.absent(&req.name)))?;
    let files = draft_files(package)?;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, text) in &files {
        zip.start_file(name, options).map_err(HiveError::internal)?;
        zip.write_all(text.as_bytes())
            .map_err(HiveError::internal)?;
    }
    let zip = zip.finish().map_err(HiveError::internal)?.into_inner();
    Ok(ExportWorkflowResponse { zip })
}

/// The files in a zip, with the one top folder a file manager adds taken off.
fn unzip(bytes: &[u8]) -> Result<BTreeMap<String, String>, HiveError> {
    let bad =
        |what: String| HiveError::InvalidArgument(format!("the zip cannot be imported: {what}"));
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| bad(e.to_string()))?;
    if archive.len() > MAX_DRAFT_FILES {
        return Err(bad(format!(
            "it holds {} entries, over the {MAX_DRAFT_FILES} limit",
            archive.len()
        )));
    }
    let mut files = BTreeMap::new();
    let mut total = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| bad(e.to_string()))?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_owned();
        // The declared size is the zip's claim, so the read is capped too.
        let mut text = Vec::new();
        entry
            .take((MAX_DRAFT_BYTES - total + 1) as u64)
            .read_to_end(&mut text)
            .map_err(|e| bad(format!("{name}: {e}")))?;
        total += text.len();
        if total > MAX_DRAFT_BYTES {
            return Err(bad(format!(
                "its files are over the {} MiB limit",
                MAX_DRAFT_BYTES >> 20
            )));
        }
        let text = String::from_utf8(text).map_err(|_| bad(format!("{name} is not text")))?;
        files.insert(name, text);
    }
    if !files.contains_key("kiln-package.yaml") {
        let tops: std::collections::BTreeSet<&str> = files
            .keys()
            .filter_map(|n| n.split_once('/').map(|(top, _)| top))
            .collect();
        let only = (tops.len() == 1 && files.keys().all(|n| n.contains('/')))
            .then(|| tops.into_iter().next())
            .flatten();
        if let Some(top) = only.map(str::to_owned) {
            files = files
                .into_iter()
                .filter_map(|(n, t)| {
                    n.strip_prefix(&format!("{top}/"))
                        .map(|n| (n.to_owned(), t))
                })
                .collect();
        }
    }
    if !files.contains_key("kiln-package.yaml") {
        return Err(bad("kiln-package.yaml is not at its top".into()));
    }
    Ok(files)
}

pub async fn import_workflow(
    app: App,
    ctx: Ctx,
    req: ImportWorkflowRequest,
) -> Result<Workflow, HiveError> {
    let mut files = unzip(&req.zip)?;
    if !req.name.is_empty() {
        files = renamed(&files, &req.name)?;
    }
    let name = buzz_hive_core::workflow::load::parse("zip", files.clone())
        .map_err(|e| HiveError::InvalidArgument(format!("the zip does not decode: {e}")))?
        .meta
        .name;
    let package = draft_of(
        &name,
        files
            .into_iter()
            .map(|(n, t)| (n, t.into_bytes()))
            .collect(),
    )?;
    save(
        &app.op(false)?.env(),
        &ctx.op(),
        &saver(&ctx),
        package,
        None,
    )
    .await?;
    app.refresh_saved().await?;
    listed(&app, &ctx, &name).await
}

/// The agent's `edit_workflow`: a save when the person it acts for may save, else a proposal.
pub async fn edit_workflow(
    app: App,
    ctx: Ctx,
    req: EditWorkflowRequest,
) -> Result<EditWorkflowResponse, HiveError> {
    let package = draft_of(
        &req.name,
        req.files
            .iter()
            .map(|(n, t)| (n.clone(), t.clone().into_bytes()))
            .collect(),
    )?;
    let base = Some(req.base_hash.as_str()).filter(|h| !h.is_empty());
    let op = app.op(false)?;
    let env = op.env();
    let saver = saver(&ctx);
    // An agent acts at the lower of its own level and its requester's, and one with no claim
    // is held to Read, so it may only propose.
    if allow(ctx.actor.level, ctx.requester, Level::Run).is_ok() {
        let saved = edit(&env, &ctx.op(), &saver, package, base).await?;
        app.refresh_saved().await?;
        return Ok(EditWorkflowResponse {
            saved: true,
            hash: saved.hash,
            findings: to_findings(&saved.findings),
            ..Default::default()
        });
    }
    let (id, findings) = propose_workflow(&env, &ctx.op(), &saver.pubkey, package, base).await?;
    Ok(EditWorkflowResponse {
        proposal_id: id,
        findings: to_findings(&findings),
        ..Default::default()
    })
}
