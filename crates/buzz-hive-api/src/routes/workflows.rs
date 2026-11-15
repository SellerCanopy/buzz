//! Workflow procedures: list the packages, check them and offer what a draft may use.

use std::collections::BTreeMap;

use buzz_hive_core::domain::workflow::{Finding, Stage};
use buzz_hive_core::workflow::actions::{stage_needs_code_host, stage_needs_issue_host};
use buzz_hive_core::workflow::builtin::{builtin_templates, ENGINE_VERSION};
use buzz_hive_core::workflow::check::{check, has_errors, CheckEnv};
use buzz_hive_core::workflow::load::{clean_path, is_junk_name, parse, valid_path, Package};
use buzz_hive_core::workflow::options::resolve_options;
use buzz_hive_core::workflow::template::{draft_variables, variables};
use buzz_hive_core::workflow::templates::load_stage_templates;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    CheckWorkflowRequest, CheckWorkflowResponse, ListStageTemplatesRequest,
    ListStageTemplatesResponse, ListTemplateVariablesRequest, ListTemplateVariablesResponse,
    ListWorkflowsRequest, ListWorkflowsResponse, PackageCheck, StageTemplate, TemplateVariable,
    Workflow, WorkflowDraft, WorkflowStage,
};
use crate::unary::Ctx;

pub async fn list_workflows(
    app: App,
    ctx: Ctx,
    _req: ListWorkflowsRequest,
) -> Result<ListWorkflowsResponse, HiveError> {
    let catalog = app.catalog.load();
    let options = {
        let mut tx = app.store.begin(ctx.community).await?;
        tx.workflows().options().await?
    };
    let env = CheckEnv {
        catalog: Some(&catalog),
    };
    let mut workflows = Vec::new();
    for name in catalog.names() {
        let packages = catalog.all(name);
        let first = &packages[0];
        let ok = packages.iter().all(|p| !has_errors(&check(p, &env)));
        let resolved = resolve_options(first, options.get(name));
        let stages = &first.workflow.stages;
        workflows.push(Workflow {
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
            own: false,
            needs_issue_host: stages.iter().any(stage_needs_issue_host),
            needs_code_host: stages.iter().any(stage_needs_code_host),
            paused_stages: resolved.paused,
            next_workflow: resolved.next,
            labels: resolved.labels,
            stages: stages.iter().map(stage_to_proto).collect(),
        });
    }
    Ok(ListWorkflowsResponse { workflows })
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
    _ctx: Ctx,
    req: CheckWorkflowRequest,
) -> Result<CheckWorkflowResponse, HiveError> {
    if let Some(draft) = &req.draft {
        let package = draft_package(draft)?;
        let findings = check(&package, &CheckEnv::default());
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
        findings: findings
            .iter()
            .map(|f| crate::proto::Finding {
                path: f.path.clone(),
                code: f.code.clone(),
                severity: f.severity.as_str().to_owned(),
                message: f.message.clone(),
            })
            .collect(),
    }
}

/// The package a draft holds. A bad file name, a file that is not text or a name that
/// differs from the one in `kiln-package.yaml` is `InvalidArgument`.
fn draft_package(draft: &WorkflowDraft) -> Result<Package, HiveError> {
    let mut files = BTreeMap::new();
    for (name, bytes) in &draft.files {
        if !valid_path(name) || clean_path(name) != *name || name.split('/').any(is_junk_name) {
            return Err(HiveError::InvalidArgument(format!(
                "{name:?} is not a file name a package can hold"
            )));
        }
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
            HiveError::InvalidArgument(format!("{name} is not text, and a package holds text"))
        })?;
        files.insert(name.clone(), text);
    }
    let package = parse(&format!("draft/{}", draft.name), files)
        .map_err(|e| HiveError::InvalidArgument(format!("the draft does not decode: {e}")))?;
    if package.meta.name != draft.name {
        return Err(HiveError::InvalidArgument(format!(
            "the draft is named {:?} but its kiln-package.yaml names {:?}",
            draft.name, package.meta.name
        )));
    }
    Ok(package)
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
