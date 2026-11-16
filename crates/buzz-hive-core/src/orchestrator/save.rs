//! Saving the workflows people write: a new version of a package, by content hash.
//!
//! Ported from the Go engine's `save.go`. A save never touches a run: each run keeps the hash
//! it started with and loads that version (see [`crate::workflow::load::Catalog::pinned`]).
//! The store keeps every version, so an edit never takes one away from a card in flight.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::workflow::{Finding, Severity, PACKAGE_FILE, WORKFLOW_FILE};
use crate::orchestrator::run::people_only;
use crate::orchestrator::{Ctx, Env, OpError};
use crate::ports::store::{AgentInfo, Pubkey};
use crate::workflow::check::{check, has_errors, CheckEnv};
use crate::workflow::load::{clean_path, is_junk_name, parse, valid_path, Package};
use crate::workflow::names::{agent_name, package_name, PACKAGE_NAME_RULE};
use crate::workflow::options::WorkflowOptions;
use crate::workflow::saved::SAVED_LABEL;

/// The most files a draft holds.
pub const MAX_DRAFT_FILES: usize = 500;
/// The most bytes a draft's files hold together.
pub const MAX_DRAFT_BYTES: usize = 4 << 20;

/// The refusal for a save from a version that is no longer the current one.
pub const STALE: &str = "the workflow changed since you opened it; reload and try again";

/// Who saves, for the check that they may name each agent.
#[derive(Debug, Clone)]
pub struct Saver {
    /// The person: the caller, or the requester an agent acts for.
    pub pubkey: Pubkey,
    /// The person is at Admin, who may name any agent.
    pub admin: bool,
}

/// A workflow that was saved.
#[derive(Debug, Clone)]
pub struct Saved {
    pub name: String,
    /// The hash of the files written, which the next save names as its base.
    pub hash: String,
    /// Warnings and notes the checks found. A draft with an error is never saved.
    pub findings: Vec<Finding>,
}

/// The package a draft holds. A bad file name, a file that is not text, too many or too large
/// files, or a name that differs from the one in `kiln-package.yaml` is `InvalidArgument`.
pub fn draft_package(name: &str, files: Vec<(String, Vec<u8>)>) -> Result<Package, OpError> {
    if files.len() > MAX_DRAFT_FILES {
        return Err(OpError::InvalidArgument(format!(
            "the package holds {} files, over the {MAX_DRAFT_FILES} file limit",
            files.len()
        )));
    }
    let mut total = 0;
    let mut text = BTreeMap::new();
    for (path, bytes) in files {
        if !valid_path(&path) || clean_path(&path) != path || path.split('/').any(is_junk_name) {
            return Err(OpError::InvalidArgument(format!(
                "{path:?} is not a file name a package can hold"
            )));
        }
        total += bytes.len();
        let body = String::from_utf8(bytes).map_err(|_| {
            OpError::InvalidArgument(format!("{path} is not text, and a package holds text"))
        })?;
        text.insert(path, body);
    }
    if total > MAX_DRAFT_BYTES {
        return Err(OpError::InvalidArgument(format!(
            "the package files are {total} bytes, over the {} MiB limit",
            MAX_DRAFT_BYTES >> 20
        )));
    }
    let package = parse(&format!("draft/{name}"), text)
        .map_err(|e| OpError::InvalidArgument(format!("the draft does not decode: {e}")))?;
    if package.meta.name != name {
        return Err(OpError::InvalidArgument(format!(
            "the draft is named {name:?} but its {PACKAGE_FILE} names {:?}",
            package.meta.name
        )));
    }
    Ok(package)
}

/// A package's files as a draft holds them: `kiln-package.yaml` and `workflow.yaml` as JSON
/// text, every other file as it is. The draft must read back as the same package.
pub fn draft_files(package: &Package) -> Result<BTreeMap<String, String>, OpError> {
    let unreadable = |what: String| {
        OpError::FailedPrecondition(format!("{} cannot open as a draft: {what}", package.path))
    };
    let mut files = package.files.clone();
    for name in [PACKAGE_FILE, WORKFLOW_FILE] {
        let Some(text) = package.files.get(name) else {
            continue;
        };
        let value: serde_json::Value =
            serde_yaml::from_str(text).map_err(|e| unreadable(format!("{name}: {e}")))?;
        let json =
            serde_json::to_string_pretty(&value).map_err(|e| unreadable(format!("{name}: {e}")))?;
        files.insert(name.to_owned(), json);
    }
    let back = parse(&package.path, files.clone()).map_err(&unreadable)?;
    if back.meta != package.meta || back.workflow != package.workflow {
        return Err(unreadable(
            "its files would read back differently as JSON".into(),
        ));
    }
    Ok(files)
}

/// `files` with the package renamed to `name` in its `kiln-package.yaml`.
pub fn renamed(
    files: &BTreeMap<String, String>,
    name: &str,
) -> Result<BTreeMap<String, String>, OpError> {
    let bad = |what: String| OpError::InvalidArgument(format!("{PACKAGE_FILE}: {what}"));
    let text = files
        .get(PACKAGE_FILE)
        .ok_or_else(|| bad("is missing".into()))?;
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| bad(e.to_string()))?;
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| bad("is not a mapping".into()))?;
    map.insert("name".into(), name.into());
    let mut out = files.clone();
    out.insert(
        PACKAGE_FILE.to_owned(),
        serde_yaml::to_string(&value).map_err(|e| bad(e.to_string()))?,
    );
    Ok(out)
}

/// The Buzz agents each stage of `package` names, by name, as the relay lists them.
async fn agents_of(
    env: &Env<'_>,
    ctx: &Ctx,
    package: &Package,
) -> Result<BTreeMap<String, Vec<AgentInfo>>, OpError> {
    let mut found = BTreeMap::new();
    for stage in &package.workflow.stages {
        let Some(name) = stage.agent.as_deref().filter(|n| agent_name(n)) else {
            continue;
        };
        if !found.contains_key(name) {
            found.insert(
                name.to_owned(),
                env.directory.agents_named(ctx.community, name).await?,
            );
        }
    }
    Ok(found)
}

/// Every finding for a draft: what the checks find in its files, and an `agent_unknown` for
/// each stage whose agent no Buzz agent has the name of.
pub async fn check_draft(
    env: &Env<'_>,
    ctx: &Ctx,
    package: &Package,
) -> Result<Vec<Finding>, OpError> {
    let agents = agents_of(env, ctx, package).await?;
    Ok(findings_of(package, &agents))
}

fn findings_of(package: &Package, agents: &BTreeMap<String, Vec<AgentInfo>>) -> Vec<Finding> {
    let mut findings = check(package, &CheckEnv::default());
    for (i, stage) in package.workflow.stages.iter().enumerate() {
        let Some(name) = stage.agent.as_deref() else {
            continue;
        };
        if agents.get(name).is_some_and(Vec::is_empty) {
            findings.push(Finding::error(
                format!("{WORKFLOW_FILE}:stages[{i}].agent"),
                "agent_unknown",
                format!("no Buzz agent is named {name}; create it in Buzz, or name another agent"),
            ));
        }
    }
    findings
}

/// Refuses an agent the saver may not name, naming it. A saver may name the agents they own,
/// and an Admin any agent. Only an agent the draft adds or changes is looked at, so an edit
/// to another stage, or a copy, does not fail on an agent the saver never chose.
fn reach_check(
    saver: &Saver,
    package: &Package,
    known: Option<&Package>,
    agents: &BTreeMap<String, Vec<AgentInfo>>,
) -> Result<(), OpError> {
    if saver.admin {
        return Ok(());
    }
    let accepted: BTreeSet<&str> = known
        .into_iter()
        .flat_map(|p| p.workflow.stages.iter())
        .filter_map(|s| s.agent.as_deref())
        .collect();
    for stage in &package.workflow.stages {
        let Some(name) = stage.agent.as_deref() else {
            continue;
        };
        if accepted.contains(name) {
            continue;
        }
        let owned = agents
            .get(name)
            .into_iter()
            .flatten()
            .any(|agent| agent.owner.as_deref() == Some(saver.pubkey.as_str()));
        if !owned {
            return Err(OpError::PermissionDenied(format!(
                "you cannot use the agent {name} in stage {}: you do not own it. Name an agent you own, or ask its owner or an admin",
                stage.id
            )));
        }
    }
    Ok(())
}

/// The package a save changes, which must be the saver's own and at `base`.
fn current_of<'a>(env: &'a Env<'_>, name: &str, base: &str) -> Result<&'a Package, OpError> {
    let current = env
        .catalog
        .get(name)
        .ok_or_else(|| OpError::NotFound(env.catalog.absent(name)))?;
    own(current)?;
    if current.hash() != base {
        return Err(OpError::FailedPrecondition(STALE.into()));
    }
    Ok(current)
}

/// Refuses a package a person cannot change: a built-in, or one loaded from a folder.
fn own(package: &Package) -> Result<(), OpError> {
    let name = &package.meta.name;
    if package.builtin {
        return Err(OpError::PermissionDenied(format!(
            "workflow {name} is built in and cannot be changed; duplicate it and change the copy"
        )));
    }
    if !package.path.starts_with(&format!("{SAVED_LABEL}/")) {
        return Err(OpError::PermissionDenied(format!(
            "workflow {name} is loaded from a folder on the server and cannot be changed here; duplicate it and change the copy"
        )));
    }
    Ok(())
}

/// Saves `draft` as a version of the package of its name and makes it current. `base` is the
/// hash the draft was opened at; `None` makes a new package. `known` is a package whose agents
/// the saver already has the right to, such as the one a copy comes from.
///
/// The caller has checked the saver's level. A draft with an error is `InvalidWorkflow` and
/// writes nothing; a package that is no longer at `base` is `FailedPrecondition`.
async fn save(
    env: &Env<'_>,
    ctx: &Ctx,
    saver: &Saver,
    draft: Package,
    base: Option<&str>,
    known: Option<&Package>,
) -> Result<Saved, OpError> {
    let name = draft.meta.name.clone();
    if !package_name(&name) {
        return Err(OpError::InvalidArgument(format!(
            "the name {name:?} is not allowed: use {PACKAGE_NAME_RULE}"
        )));
    }
    let known = match base {
        Some(base) => Some(current_of(env, &name, base)?),
        None => {
            if !env.catalog.all(&name).is_empty() {
                return Err(OpError::AlreadyExists(format!(
                    "a workflow named {name} already exists"
                )));
            }
            known
        }
    };
    let agents = agents_of(env, ctx, &draft).await?;
    let findings = findings_of(&draft, &agents);
    if has_errors(&findings) {
        return Err(OpError::InvalidWorkflow(findings));
    }
    reach_check(saver, &draft, known, &agents)?;
    let hash = draft.hash();
    let body = serde_json::to_value(&draft.files)
        .map_err(|e| OpError::InvalidArgument(format!("the files cannot be stored: {e}")))?;
    let mut tx = env.store.begin(ctx.community).await?;
    tx.workflows()
        .save_version(&name, base, &hash, body, &saver.pubkey)
        .await?;
    tx.commit().await?;
    Ok(Saved {
        name,
        hash,
        findings: findings
            .into_iter()
            .filter(|f| f.severity != Severity::Error)
            .collect(),
    })
}

/// A person saves a draft: a new package when `base` is `None`, else a new version of their
/// own package that is still at `base`.
pub async fn save_workflow(
    env: &Env<'_>,
    ctx: &Ctx,
    saver: &Saver,
    draft: Package,
    base: Option<&str>,
) -> Result<Saved, OpError> {
    people_only(ctx, "save a workflow")?;
    save(env, ctx, saver, draft, base, None).await
}

/// An agent's edit, saved like a person's. The caller has found that the agent's requester
/// may run workflows, and otherwise uses [`propose_workflow`].
pub async fn edit_workflow(
    env: &Env<'_>,
    ctx: &Ctx,
    saver: &Saver,
    draft: Package,
    base: Option<&str>,
) -> Result<Saved, OpError> {
    save(env, ctx, saver, draft, base, None).await
}

/// Copies the loaded workflow `name` as a new package called `new_name`, changing nothing else.
pub async fn duplicate_workflow(
    env: &Env<'_>,
    ctx: &Ctx,
    saver: &Saver,
    name: &str,
    new_name: &str,
) -> Result<Saved, OpError> {
    people_only(ctx, "duplicate a workflow")?;
    let source = env
        .catalog
        .get(name)
        .ok_or_else(|| OpError::NotFound(env.catalog.absent(name)))?;
    if !package_name(new_name) {
        return Err(OpError::InvalidArgument(format!(
            "the name {new_name:?} is not allowed: use {PACKAGE_NAME_RULE}"
        )));
    }
    let files = renamed(&source.files, new_name)?;
    let copy = parse(&format!("draft/{new_name}"), files)
        .map_err(|e| OpError::InvalidArgument(format!("the copy does not decode: {e}")))?;
    save(env, ctx, saver, copy, None, Some(source)).await
}

/// Deletes the person's own package `name`. Runs that started on it keep their version. A
/// package a card or an intake rule uses is refused when it is the last copy loaded.
pub async fn delete_workflow(env: &Env<'_>, ctx: &Ctx, name: &str) -> Result<(), OpError> {
    people_only(ctx, "delete a workflow")?;
    let loaded = env.catalog.all(name);
    let first = loaded
        .first()
        .ok_or_else(|| OpError::NotFound(env.catalog.absent(name)))?;
    // A copy that is not the person's own is not theirs to delete, even beside one that is.
    if !loaded.iter().any(|p| own(p).is_ok()) {
        own(first)?;
    }
    let mut tx = env.store.begin(ctx.community).await?;
    if loaded.len() == 1 {
        let cards = tx
            .cards()
            .list(false)
            .await?
            .into_iter()
            .filter(|card| card.workflow == name)
            .count();
        if cards > 0 {
            return Err(OpError::FailedPrecondition(format!(
                "{cards} card{} on the board use{} workflow {name}; move {} first",
                if cards == 1 { "" } else { "s" },
                if cards == 1 { "s" } else { "" },
                if cards == 1 { "it" } else { "them" },
            )));
        }
        let rules: Vec<String> = tx
            .intake()
            .list()
            .await?
            .into_iter()
            .filter(|rule| rule.workflow == name)
            .map(|rule| rule.name)
            .collect();
        if !rules.is_empty() {
            return Err(OpError::FailedPrecondition(format!(
                "intake rule {} brings issues onto workflow {name}; change the rule first",
                rules.join(", ")
            )));
        }
    }
    tx.workflows().delete_package(name).await?;
    tx.commit().await?;
    Ok(())
}

/// Replaces the board behavior of the loaded workflow `name`. It works on a built-in too.
pub async fn set_workflow_options(
    env: &Env<'_>,
    ctx: &Ctx,
    name: &str,
    options: WorkflowOptions,
) -> Result<(), OpError> {
    people_only(ctx, "change a workflow's options")?;
    let package = env
        .catalog
        .get(name)
        .ok_or_else(|| OpError::NotFound(env.catalog.absent(name)))?;
    for id in options.paused_stages.iter().flatten() {
        if package.stage(id).is_none() {
            return Err(OpError::InvalidArgument(format!(
                "workflow {name} has no stage {id}"
            )));
        }
    }
    let mut tx = env.store.begin(ctx.community).await?;
    if !options.next_workflow.is_empty() {
        if env.catalog.get(&options.next_workflow).is_none() {
            return Err(OpError::InvalidArgument(format!(
                "the next workflow {} does not exist",
                options.next_workflow
            )));
        }
        let stored = tx.workflows().options().await?;
        let mut seen = BTreeSet::new();
        let mut at = options.next_workflow.as_str();
        loop {
            if at == name {
                return Err(OpError::InvalidArgument(format!(
                    "the next workflow {} hands off back to {name}",
                    options.next_workflow
                )));
            }
            if !seen.insert(at) {
                break;
            }
            match stored.get(at).map(|o| o.next_workflow.as_str()) {
                Some(next) if !next.is_empty() => at = next,
                _ => break,
            }
        }
    }
    tx.workflows().set_options(name, &options).await?;
    tx.commit().await?;
    Ok(())
}

/// Stores a workflow edit an agent made for a person who may not save it, for that person to
/// look at. A draft with an error is refused, as for a save.
pub async fn propose_workflow(
    env: &Env<'_>,
    ctx: &Ctx,
    requester: &str,
    draft: Package,
    base: Option<&str>,
) -> Result<(i64, Vec<Finding>), OpError> {
    let findings = check_draft(env, ctx, &draft).await?;
    if has_errors(&findings) {
        return Err(OpError::InvalidWorkflow(findings));
    }
    let body = serde_json::json!({
        "name": draft.meta.name,
        "files": draft.files,
        "baseHash": base,
    });
    let mut tx = env.store.begin(ctx.community).await?;
    let id = tx
        .proposals()
        .insert(PROPOSAL_WORKFLOW, body, requester)
        .await?;
    tx.commit().await?;
    Ok((id, findings))
}

/// The `kind` of a stored proposal that holds a workflow edit.
pub const PROPOSAL_WORKFLOW: &str = "workflow";
