//! The one validator: every problem in a package, never only the first.
//!
//! Ported from the Go engine's `core/workflow/check.go`. Paths are inside the package,
//! such as `workflow.yaml:stages[1].on.passed`. Codes that depended on the Go engine's
//! agent profiles and skills are gone: a stage names one Buzz agent, and the check here
//! validates only the shape of that name. Whether the agent exists is asked of Buzz when
//! a run starts.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::workflow::{
    Finding, Route, Severity, Stage, Workspace, KIND_APPROVAL, KIND_PROMPT, KIND_WAIT,
    PACKAGE_FILE, RESULT_PASSED, ROLES, STAGE_KINDS, STANDARD_RESULTS, WORKFLOW_FILE,
};
use crate::workflow::contents::{contents_of, Contents};
use crate::workflow::kinds::check_kind;
use crate::workflow::load::{clean_path, valid_path, Catalog, Package};
use crate::workflow::names::{
    agent_name, artifact_name, check_param_value, command_name, go_list, ARTIFACT_NAME_RULE,
};
use crate::workflow::routes::{allowed_results, ROUTE_ASK, ROUTE_DONE, ROUTE_STOP};
use crate::workflow::template::{
    find_vars, input_ref, known_variable, known_variables_text, param_variable, replace_vars,
};

/// What the checks compare a package against.
#[derive(Debug, Default, Clone, Copy)]
pub struct CheckEnv<'a> {
    /// Where the package was loaded, for name clashes. `None` skips the clash check.
    pub catalog: Option<&'a Catalog>,
}

/// Whether any finding is an error.
pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

/// Every problem in `package`: what loading its files found, a name shared with another
/// package, and each rule below per stage.
pub fn check(package: &Package, env: &CheckEnv<'_>) -> Vec<Finding> {
    let mut out = package.findings.clone();
    if let Some(catalog) = env.catalog {
        let same = catalog.all(&package.meta.name);
        if same.len() > 1 {
            let paths: Vec<&str> = same.iter().map(|p| p.path.as_str()).collect();
            out.push(Finding::error(
                format!("{PACKAGE_FILE}:name"),
                "name_clash",
                format!(
                    "workflow name {:?} is used by more than one package: {}. Rename one; neither can start until then",
                    package.meta.name,
                    paths.join(", ")
                ),
            ));
        }
    }
    out.extend(check_parameters(package));
    if !package.files.contains_key(WORKFLOW_FILE) {
        return out;
    }
    let workflow = &package.workflow;
    if workflow.version != 2 {
        out.push(Finding::error(
            format!("{WORKFLOW_FILE}:version"),
            "version_unsupported",
            format!("version must be 2, got {}", workflow.version),
        ));
    }
    // The package's contents are listed once for all stages.
    let mut listed: Option<Contents> = None;
    if workflow.stages.is_empty() {
        // A package that holds a plugin is saved before it has any stage: its commands are
        // not stages yet, and a plugin with none (only subagents, say) imports as it is.
        let mut finding = Finding::error(
            format!("{WORKFLOW_FILE}:stages"),
            "stages_missing",
            "the workflow has no stages",
        );
        let contents = listed.get_or_insert_with(|| contents_of(&package.files));
        if contents.has_plugin_content() {
            finding.severity = Severity::Warning;
        }
        out.push(finding);
    }
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, stage) in workflow.stages.iter().enumerate() {
        if !stage.id.is_empty() {
            ids.entry(stage.id.as_str()).or_insert(i);
        }
    }
    for (i, stage) in workflow.stages.iter().enumerate() {
        let mut has_command = |name: &str| {
            listed
                .get_or_insert_with(|| contents_of(&package.files))
                .has_command(name)
        };
        out.extend(check_stage(package, i, stage, &ids, &mut has_command));
    }
    out
}

/// The parameters a package declares: each name is valid and used once, only a choice has
/// choices, and each default fits its type.
fn check_parameters(package: &Package) -> Vec<Finding> {
    use crate::domain::workflow::ParamType;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, parameter) in package.meta.parameters.iter().enumerate() {
        let at = format!("{PACKAGE_FILE}:parameters[{i}]");
        if !crate::workflow::names::param_name(&parameter.name) {
            out.push(Finding::error(
                format!("{at}.name"),
                "parameter_name",
                format!(
                    "parameter name {:?} must start with a lowercase letter and use only a-z, 0-9 and _",
                    parameter.name
                ),
            ));
        } else if seen.contains(&parameter.name) {
            out.push(Finding::error(
                format!("{at}.name"),
                "parameter_duplicate",
                format!("parameter {} is declared more than once", parameter.name),
            ));
        }
        seen.insert(parameter.name.clone());
        // The decoder reports a type outside the list; nothing more to check.
        let Some(kind) = parameter.kind else { continue };
        if kind == ParamType::Choice && parameter.choices.is_empty() {
            out.push(Finding::error(
                format!("{at}.choices"),
                "parameter_choices",
                format!(
                    "parameter {} is a choice with no choices; list them in choices",
                    parameter.name
                ),
            ));
            continue;
        }
        if kind != ParamType::Choice && !parameter.choices.is_empty() {
            out.push(Finding::error(
                format!("{at}.choices"),
                "parameter_choices",
                format!(
                    "parameter {} has choices but is type {}; only a choice has choices",
                    parameter.name,
                    kind.as_str()
                ),
            ));
            continue;
        }
        if let Err(error) = check_param_value(parameter, &parameter.default) {
            out.push(Finding::error(
                format!("{at}.default"),
                "parameter_default",
                format!("the default does not fit its type: {error}"),
            ));
        }
    }
    out
}

/// Why an output file pattern cannot be used, or `None` when it can.
fn output_file_problem(pattern: &str) -> Option<String> {
    if pattern.contains('\\') {
        return Some("the file pattern uses a backslash, write a slash path".into());
    }
    if pattern.starts_with('/') {
        return Some("the file pattern is absolute, write a path inside the repository".into());
    }
    if pattern
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Some("the file pattern must stay inside the repository".into());
    }
    // Variables become a plain name before the syntax check, so only the pattern is judged.
    if !glob_syntax_ok(&replace_vars(pattern, "x")) {
        return Some("the file pattern is not valid: syntax error in pattern".into());
    }
    None
}

/// Whether `pattern` is a well formed Go `path.Match` pattern: `*`, `?`, and character
/// classes such as `[a-z]` or `[^0-9]`. A backslash escapes the next character.
fn glob_syntax_ok(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                if i + 1 >= chars.len() {
                    return false;
                }
                i += 2;
            }
            '[' => {
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                let mut ranges = 0;
                loop {
                    match chars.get(i) {
                        None => return false,
                        Some(']') if ranges > 0 => break,
                        _ => {}
                    }
                    // One class element: a character, or a range of two.
                    let Some(low) = class_char(&chars, &mut i) else {
                        return false;
                    };
                    let mut high = low;
                    if chars.get(i) == Some(&'-') {
                        i += 1;
                        let Some(h) = class_char(&chars, &mut i) else {
                            return false;
                        };
                        high = h;
                    }
                    if low > high {
                        // Go accepts this and matches nothing, so it is not a syntax error.
                    }
                    ranges += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    true
}

/// One literal character of a class, or `None` when the class is malformed here.
fn class_char(chars: &[char], i: &mut usize) -> Option<char> {
    match chars.get(*i)? {
        '-' | ']' => None,
        '\\' => {
            *i += 1;
            let c = *chars.get(*i)?;
            *i += 1;
            Some(c)
        }
        c => {
            *i += 1;
            Some(*c)
        }
    }
}

fn declared_params(package: &Package) -> BTreeSet<&str> {
    package
        .meta
        .parameters
        .iter()
        .map(|p| p.name.as_str())
        .collect()
}

fn declared_inputs(stage: &Stage) -> BTreeSet<&str> {
    stage.inputs.iter().map(|i| i.name.as_str()).collect()
}

/// Reports each template variable in `text` that is neither a known variable, a declared
/// package parameter nor an input the stage declares. Each is reported once, at `where_`.
fn check_template_text(
    declared: &BTreeSet<&str>,
    inputs: &BTreeSet<&str>,
    text: &str,
    where_: &str,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for found in find_vars(text) {
        let name = found.name;
        if !seen.insert(name) {
            continue;
        }
        if let Some(param) = param_variable(name) {
            if !declared.contains(param) {
                out.push(unknown_parameter(where_, param));
            }
            continue;
        }
        if let Some(input) = input_ref(name) {
            if !inputs.contains(input) {
                out.push(Finding::error(
                    where_,
                    "input_template_unknown",
                    format!(
                        "{{{{{name}}}}} names an input this stage does not declare; add {input} to the stage's inputs"
                    ),
                ));
            }
            continue;
        }
        if !known_variable(name) {
            out.push(Finding::error(
                where_,
                "template_unknown_variable",
                format!(
                    "{{{{{name}}}}} is not a template variable; want {}",
                    known_variables_text()
                ),
            ));
        }
    }
    out
}

fn unknown_parameter(path: &str, name: &str) -> Finding {
    Finding::error(
        path,
        "template_unknown_parameter",
        format!("{{{{params.{name}}}}} names no parameter this workflow declares"),
    )
}

/// Where a stage's prompt comes from and the template variables it uses. A command stage's
/// arguments are scanned the same way.
fn check_prompt_text(
    package: &Package,
    stage: &Stage,
    at: &dyn Fn(&str) -> String,
    has_command: &mut dyn FnMut(&str) -> bool,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut field = "prompt";
    let mut text = stage.prompt.clone();
    let set: Vec<&str> = [
        ("prompt", &stage.prompt),
        ("prompt_file", &stage.prompt_file),
        ("command", &stage.command),
    ]
    .iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(name, _)| *name)
    .collect();
    if set.len() > 1 {
        out.push(Finding::error(
            at("prompt_file"),
            "prompt_conflict",
            format!(
                "set one of prompt, prompt_file or command, not {}",
                set.join(" and ")
            ),
        ));
    } else if !stage.prompt_file.is_empty() {
        field = "prompt_file";
        let clean = clean_path(&stage.prompt_file);
        if !valid_path(&clean) {
            out.push(Finding::error(
                at(field),
                "prompt_file_outside",
                format!(
                    "prompt_file {} must be a path inside the package",
                    stage.prompt_file
                ),
            ));
        } else if let Some(body) = package.files.get(&clean) {
            text = body.clone();
        } else {
            out.push(Finding::error(
                at(field),
                "prompt_file_missing",
                format!("prompt_file {} is not in the package", stage.prompt_file),
            ));
        }
    }
    if !stage.args.is_empty() && stage.command.is_empty() {
        out.push(Finding::error(
            at("args"),
            "args_without_command",
            "args is set, but the stage has no command to send them to",
        ));
    } else if !stage.command.is_empty() && !command_name(&stage.command) {
        out.push(Finding::error(
            at("command"),
            "command_name_invalid",
            format!("command {:?} is not a bare command name", stage.command),
        ));
    } else if !stage.command.is_empty() && !has_command(&stage.command) {
        out.push(Finding::warning(
            at("command"),
            "command_missing",
            format!("this package has no command {:?}", stage.command),
        ));
    }
    let declared = declared_params(package);
    let inputs = declared_inputs(stage);
    out.extend(check_template_text(&declared, &inputs, &text, &at(field)));
    if !stage.command.is_empty() {
        out.extend(check_template_text(
            &declared,
            &inputs,
            &stage.args,
            &at("args"),
        ));
    }
    out
}

/// A `{{params.<name>}}` in an action stage's `with` arguments that names no declared
/// parameter. The argument classes are checked by the action kind; only the package knows
/// the declared names.
fn check_action_params(
    package: &Package,
    stage: &Stage,
    at: &dyn Fn(&str) -> String,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let declared = declared_params(package);
    let Some(with) = &stage.with else {
        return out;
    };
    for (key, arg) in with {
        let texts: Vec<&str> = if arg.is_list() {
            arg.list().iter().map(String::as_str).collect()
        } else {
            vec![arg.text()]
        };
        let mut seen = BTreeSet::new();
        for text in texts {
            for found in find_vars(text) {
                if let Some(param) = param_variable(found.name) {
                    if !declared.contains(param) && seen.insert(param) {
                        out.push(unknown_parameter(&at(&format!("with.{key}")), param));
                    }
                }
            }
        }
    }
    out
}

/// Applies every per-stage rule to the stage at index `i`.
fn check_stage(
    package: &Package,
    i: usize,
    stage: &Stage,
    ids: &BTreeMap<&str, usize>,
    has_command: &mut dyn FnMut(&str) -> bool,
) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let stages = &package.workflow.stages;
    let at = |field: &str| {
        let mut where_ = format!("{WORKFLOW_FILE}:stages[{i}]");
        if !field.is_empty() {
            where_.push('.');
            where_.push_str(field);
        }
        where_
    };
    macro_rules! add {
        ($field:expr, $code:expr, $($message:tt)+) => {
            out.push(Finding::error(at($field), $code, format!($($message)+)))
        };
    }

    if stage.id.is_empty() {
        add!("id", "stage_id_missing", "the stage has no id");
    } else if ids.get(stage.id.as_str()) != Some(&i) {
        add!(
            "id",
            "stage_id_duplicate",
            "stage id {} is already used by stages[{}]",
            stage.id,
            ids[stage.id.as_str()]
        );
    } else if [ROUTE_STOP, ROUTE_ASK, ROUTE_DONE].contains(&stage.id.as_str()) {
        add!(
            "id",
            "stage_id_reserved",
            "{} is a route target, so it cannot be a stage id",
            stage.id
        );
    }

    let kinds = go_list(&STAGE_KINDS);
    if stage.kind.is_empty() {
        add!(
            "kind",
            "kind_missing",
            "the stage has no kind; want one of {kinds}"
        );
    } else if !STAGE_KINDS.contains(&stage.kind.as_str()) {
        add!(
            "kind",
            "kind_unknown",
            "kind {:?} is not a stage kind; want one of {kinds}",
            stage.kind
        );
    } else {
        for mut finding in check_kind(stage) {
            finding.path = at(&finding.path);
            out.push(finding);
        }
    }

    let allowed = allowed_results(stage);
    for (j, result) in stage.results.iter().enumerate() {
        if result.is_empty() {
            add!(
                &format!("results[{j}]"),
                "result_empty",
                "a custom result has no name"
            );
        } else if !STANDARD_RESULTS.contains(&result.as_str())
            && stage.on.get(result).is_none_or(|route| route.to.is_empty())
        {
            add!(
                "on",
                "result_unrouted",
                "custom result {result} has no route; add on.{result}"
            );
        }
    }
    for (result, route) in &stage.on {
        if !allowed.contains(result) {
            add!(
                &format!("on.{result}"),
                "route_result_unknown",
                "{result} is not a result this stage can report; want one of {}",
                go_list(&allowed)
            );
        }
        let mut targets = vec![route.to.as_str()];
        if !route.then.is_empty() {
            targets.push(route.then.as_str());
        }
        for target in targets {
            if !ids.contains_key(target) && target != ROUTE_STOP && target != ROUTE_ASK {
                add!(
                    &format!("on.{result}"),
                    "route_target_unknown",
                    "route target {target:?} is not a stage id, stop or ask"
                );
            }
        }
        if let Some(&j) = ids.get(route.to.as_str()) {
            if route.is_call() {
                for problem in check_call(stages, &stages[j], route) {
                    add!(&format!("on.{result}.give"), problem.0, "{}", problem.1);
                }
            }
        }
    }
    // A call that carries a then leaves it in the pending call. When this stage then calls
    // another stage, that call replaces it, and the then is lost.
    if called_with_then(stages, &stage.id) {
        for (result, route) in &stage.on {
            if route.is_call() && route.to != stage.id {
                add!(
                    &format!("on.{result}"),
                    "call_then_replaced",
                    "{} is called with a then, and calls {}, which replaces it",
                    stage.id,
                    route.to
                );
            }
        }
    }

    out.extend(check_prompt_text(package, stage, &at, has_command));
    if stage.kind == crate::domain::workflow::KIND_ACTION {
        out.extend(check_action_params(package, stage, &at));
    }

    let declared = declared_params(package);
    let stage_inputs = declared_inputs(stage);
    let mut outputs = BTreeSet::new();
    for (j, output) in stage.outputs.iter().enumerate() {
        let field = format!("outputs[{j}].artifact");
        if !artifact_name(&output.artifact) {
            add!(
                &field,
                "artifact_name_invalid",
                "artifact name {:?} must match {ARTIFACT_NAME_RULE}",
                output.artifact
            );
        } else if outputs.contains(&output.artifact) {
            add!(
                &field,
                "output_duplicate",
                "artifact {} is declared as an output more than once",
                output.artifact
            );
        }
        outputs.insert(output.artifact.clone());
        if !output.file.is_empty() {
            let where_ = format!("outputs[{j}].file");
            if let Some(message) = output_file_problem(&output.file) {
                add!(&where_, "output_file_invalid", "{message}");
            }
            out.extend(check_template_text(
                &declared,
                &stage_inputs,
                &output.file,
                &at(&where_),
            ));
        }
        if output.optional {
            if let Some(target) = passed_needs_optional(stages, i, &output.artifact) {
                out.push(Finding::warning(
                    at(&format!("outputs[{j}].optional")),
                    "output_optional_needed",
                    format!(
                        "{} is optional, but {target}, where passed goes, needs it to start and no other stage always saves it; a pass without it would stop that start",
                        output.artifact
                    ),
                ));
            }
        }
    }
    let kind = stage.kind.as_str();
    if !stage.inputs.is_empty() && kind != KIND_PROMPT && !kind.is_empty() {
        add!(
            "inputs",
            "inputs_not_allowed",
            "only a prompt stage reads inputs, and this stage is kind {kind}"
        );
    }
    let mut inputs = BTreeSet::new();
    for (j, input) in stage.inputs.iter().enumerate() {
        let field = format!("inputs[{j}].name");
        if !artifact_name(&input.name) {
            add!(
                &field,
                "input_name_invalid",
                "input name {:?} must match {ARTIFACT_NAME_RULE}",
                input.name
            );
        } else if inputs.contains(&input.name) {
            add!(
                &field,
                "input_duplicate",
                "input {:?} is declared more than once",
                input.name
            );
        }
        inputs.insert(input.name.clone());
    }
    if stage.repeat.is_some() && kind != KIND_PROMPT && !kind.is_empty() {
        add!(
            "repeat",
            "repeat_not_allowed",
            "only a prompt stage can repeat, and this stage is kind {kind}"
        );
    }
    if stage.approval.is_some() && kind != KIND_APPROVAL && !kind.is_empty() {
        add!(
            "approval",
            "approval_not_allowed",
            "only an approval stage has an approval group, and this stage is kind {kind}"
        );
    }
    if stage.wait.is_some() && kind != KIND_WAIT && !kind.is_empty() {
        add!(
            "wait",
            "wait_not_allowed",
            "only a wait stage has a wait group, and this stage is kind {kind}"
        );
    }
    match stage.workspace {
        None => {}
        Some(_) if kind == KIND_APPROVAL || kind == KIND_WAIT => add!(
            "workspace",
            "workspace_not_allowed",
            "a {kind} stage starts no work, so it has no workspace"
        ),
        Some(Workspace::None) if kind == KIND_PROMPT => add!(
            "workspace",
            "workspace_not_allowed",
            "a prompt stage's agent works in the issue's worktree, so its workspace cannot be none"
        ),
        Some(_) => {}
    }
    if !stage.action.is_empty() && kind != crate::domain::workflow::KIND_ACTION && !kind.is_empty()
    {
        add!(
            "action",
            "action_not_allowed",
            "only an action stage names an action, and this stage is kind {kind}"
        );
    }
    if stage.with.is_some() && kind != crate::domain::workflow::KIND_ACTION && !kind.is_empty() {
        add!(
            "with",
            "with_not_allowed",
            "only an action stage has with arguments, and this stage is kind {kind}"
        );
    }
    for (j, role) in stage.roles.iter().enumerate() {
        if !ROLES.contains(&role.as_str()) {
            add!(
                &format!("roles[{j}]"),
                "unknown_role",
                "role {role:?} is not a stage role; want one of {}",
                go_list(&ROLES)
            );
        }
    }
    if let Some(conditions) = &stage.conditions {
        if conditions.labels.is_empty() {
            add!(
                "conditions.labels",
                "conditions_labels_missing",
                "conditions name the labels the issue must have in conditions.labels"
            );
        }
        let target = conditions.else_to.as_str();
        if target.is_empty() {
            add!(
                "conditions.else",
                "conditions_else_missing",
                "conditions need an else route: a stage id, stop or ask"
            );
        } else if !ids.contains_key(target) && target != ROUTE_STOP && target != ROUTE_ASK {
            add!(
                "conditions.else",
                "route_target_unknown",
                "route target {target:?} is not a stage id, stop or ask"
            );
        }
    }
    if stage.loop_limit == 0 {
        for (result, route) in &stage.on {
            if ids.get(route.to.as_str()).is_some_and(|&j| j <= i) {
                out.push(Finding::warning(
                    at(&format!("on.{result}")),
                    "route_cycle_unbounded",
                    format!(
                        "{result} routes back to {} with no loop_limit, so the loop can run without end; set loop_limit",
                        route.to
                    ),
                ));
            }
        }
    }

    if stage.loop_limit < 0 {
        add!(
            "loop_limit",
            "count_negative",
            "loop_limit must be 0 or more"
        );
    }
    if stage.execution.retries.max < 0 {
        add!(
            "execution.retries.max",
            "count_negative",
            "retries.max must be 0 or more"
        );
    }
    for (field, value) in [
        ("execution.timeout", stage.execution.timeout),
        ("execution.retries.backoff", stage.execution.retries.backoff),
    ] {
        if value.nanos() < 0 {
            add!(field, "duration_negative", "{field} must be zero or more");
        }
    }

    if kind == KIND_PROMPT {
        out.extend(check_agent(package, i, stage));
    }
    out
}

/// A prompt stage names one Buzz agent. Whether the agent exists is Buzz's to say, when a
/// run starts; this checks only that the name could be one. A stage written the old way, with
/// an `agent` mapping, already has the finding `agent_object_removed`, so it gets no second one.
fn check_agent(package: &Package, i: usize, stage: &Stage) -> Option<Finding> {
    let path = format!("{WORKFLOW_FILE}:stages[{i}].agent");
    match stage.agent.as_deref() {
        None | Some("") => {
            let removed = package
                .findings
                .iter()
                .any(|f| f.code == "agent_object_removed" && f.path == path);
            (!removed).then(|| {
                Finding::error(
                    path,
                    "agent_unknown",
                    "the stage names no agent; set agent to the name of a Buzz agent",
                )
            })
        }
        Some(name) if !agent_name(name) => Some(Finding::error(
            path,
            "agent_name_invalid",
            format!(
                "agent {name:?} is not a Buzz agent name: use 1 to 64 characters with no control characters or surrounding spaces"
            ),
        )),
        Some(_) => None,
    }
}

type Problem = (&'static str, String);

/// The rules for a route that calls `target` with `route.give`. Each name given must be an
/// input of `target`, and the document it gives must have a valid artifact name. Each
/// required input of `target` that the call leaves out needs a stage in the workflow that
/// saves a document of that name, since the input then reads the document of its own name.
fn check_call(stages: &[Stage], target: &Stage, route: &Route) -> Vec<Problem> {
    let mut out = Vec::new();
    for (name, artifact) in &route.give {
        if !target.inputs.iter().any(|input| &input.name == name) {
            out.push((
                "input_unknown",
                format!("{} does not take an input named {name:?}", target.id),
            ));
        } else if !artifact_name(artifact) {
            out.push((
                "give_artifact_invalid",
                format!(
                    "the document given to input {name:?} is {artifact:?}, which must match {ARTIFACT_NAME_RULE}"
                ),
            ));
        }
    }
    for input in &target.inputs {
        if !input.required || route.give.contains_key(&input.name) {
            continue;
        }
        if !saved_by_any_stage(stages, &input.name) {
            out.push((
                "input_missing",
                format!(
                    "{} needs input {:?}, which this call does not give and no stage saves",
                    target.id, input.name
                ),
            ));
        }
    }
    out
}

/// Whether some route calls the stage `id` and carries a then.
fn called_with_then(stages: &[Stage], id: &str) -> bool {
    stages.iter().any(|s| {
        s.on.values()
            .any(|route| !route.then.is_empty() && route.to == id)
    })
}

/// The stage the passed route of the stage at `i` leads to, when it has a required input that
/// reads artifact `name` and no other stage declares `name` as an output that is not
/// optional. A stage that passed having skipped an optional document would leave the start of
/// the next stage refused, so the document is not really optional.
fn passed_needs_optional<'a>(stages: &'a [Stage], i: usize, name: &str) -> Option<&'a str> {
    let route = stages[i].on.get(RESULT_PASSED);
    let target: &str = match route {
        Some(route) if !route.to.is_empty() => &route.to,
        // Without an on.passed, a pass goes to the next stage in file order.
        _ => &stages.get(i + 1)?.id,
    };
    let j = stages.iter().position(|s| s.id == target)?;
    let reads = stages[j].inputs.iter().any(|input| {
        if !input.required {
            return false;
        }
        match route.and_then(|r| r.give.get(&input.name)) {
            Some(given) => given == name,
            None => input.name == name,
        }
    });
    if !reads {
        return None;
    }
    let elsewhere = stages.iter().enumerate().any(|(k, other)| {
        k != i
            && other
                .outputs
                .iter()
                .any(|o| o.artifact == name && !o.optional)
    });
    (!elsewhere).then_some(stages[j].id.as_str())
}

/// Whether some stage declares artifact `name` as an output.
fn saved_by_any_stage(stages: &[Stage], name: &str) -> bool {
    stages
        .iter()
        .any(|s| s.outputs.iter().any(|o| o.artifact == name))
}
