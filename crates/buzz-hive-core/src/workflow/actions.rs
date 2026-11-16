//! The built-in actions an action stage can name: their arguments and what they need.
//!
//! Ported from the Go engine's `core/kinds/action.go` (the argument table and its checks)
//! and `core/rules/actions.go` (the facts about each action). Running an action is a later
//! phase; this is what the checks and the workflow list need.

use std::collections::BTreeMap;

use crate::domain::workflow::{
    ActionArg, Duration, Finding, Stage, KIND_ACTION, KIND_WAIT, ROLE_CLOSES_ISSUE, ROLE_MERGES,
    ROLE_OPENS_PULL_REQUEST, WAIT_PULL_REQUEST_STATES,
};
use crate::workflow::names::repo_name;
use crate::workflow::template::{
    find_vars, has_var, known_variable, known_variables_text, param_variable, value_variable,
};

/// Which template variables an action argument may render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Text the host stores as text, such as a pull request body. It may render every
    /// variable, issue text included.
    Text,
    /// Becomes a path, a login, a label, a repo, a number or a name. It may render only
    /// the variables the engine owns.
    Value,
    /// A command's argument list. Each element renders like a value.
    Argv,
}

struct Arg {
    name: &'static str,
    class: Class,
    required: bool,
}

const fn text(name: &'static str) -> Arg {
    Arg {
        name,
        class: Class::Text,
        required: false,
    }
}
const fn value(name: &'static str) -> Arg {
    Arg {
        name,
        class: Class::Value,
        required: false,
    }
}
const fn required(name: &'static str, class: Class) -> Arg {
    Arg {
        name,
        class,
        required: true,
    }
}

/// One built-in action, with what the engine outside the action kind needs to know.
struct Action {
    name: &'static str,
    args: &'static [Arg],
    /// The role every stage running the action holds.
    role: Option<&'static str>,
    /// It runs in the issue's worktree.
    worktree: bool,
    /// It waits with no session, like a wait stage.
    waiting: bool,
    /// It calls the code host: a pull request, CI, a merge, or the issue's git branch.
    code_host: bool,
    /// It changes the card's issue on its tracker.
    issue_host: bool,
}

const NONE: Action = Action {
    name: "",
    args: &[],
    role: None,
    worktree: false,
    waiting: false,
    code_host: false,
    issue_host: false,
};

/// Every built-in action, sorted by name.
static ACTIONS: &[Action] = &[
    Action {
        name: "assign",
        args: &[value("login")],
        issue_host: true,
        ..NONE
    },
    Action {
        name: "check_openspec",
        args: &[value("proposal"), value("specs"), value("repo")],
        worktree: true,
        ..NONE
    },
    Action {
        name: "cleanup_worktree",
        worktree: true,
        code_host: true,
        ..NONE
    },
    Action {
        name: "close_issue",
        role: Some(ROLE_CLOSES_ISSUE),
        issue_host: true,
        ..NONE
    },
    Action {
        name: "close_pull_request",
        code_host: true,
        ..NONE
    },
    Action {
        name: "copy_artifact_to_repo",
        args: &[
            required("artifact", Class::Value),
            required("path", Class::Value),
            value("repo"),
        ],
        worktree: true,
        ..NONE
    },
    Action {
        name: "create_issue",
        args: &[
            required("title", Class::Text),
            text("body"),
            value("body_artifact"),
            value("labels"),
            value("repo"),
        ],
        issue_host: true,
        ..NONE
    },
    Action {
        name: "mark_ready",
        code_host: true,
        ..NONE
    },
    Action {
        name: "merge",
        args: &[value("method")],
        role: Some(ROLE_MERGES),
        code_host: true,
        ..NONE
    },
    Action {
        name: "open_pull_request",
        args: &[
            text("title"),
            text("body"),
            value("body_artifact"),
            value("draft"),
            value("base"),
        ],
        role: Some(ROLE_OPENS_PULL_REQUEST),
        worktree: true,
        code_host: true,
        ..NONE
    },
    Action {
        name: "post_comment",
        args: &[text("body"), value("artifact")],
        issue_host: true,
        ..NONE
    },
    Action {
        name: "reply_review_comment",
        args: &[
            required("comment", Class::Value),
            required("body", Class::Text),
            value("repo"),
        ],
        code_host: true,
        ..NONE
    },
    Action {
        name: "request_reviewers",
        args: &[required("reviewers", Class::Value)],
        code_host: true,
        ..NONE
    },
    Action {
        name: "resolve_review_thread",
        args: &[required("thread", Class::Value), value("repo")],
        code_host: true,
        ..NONE
    },
    Action {
        name: "run_command",
        args: &[required("argv", Class::Argv), value("repo")],
        worktree: true,
        ..NONE
    },
    Action {
        name: "set_labels",
        args: &[value("add"), value("remove")],
        issue_host: true,
        ..NONE
    },
    Action {
        name: "sync_base",
        args: &[value("method")],
        worktree: true,
        code_host: true,
        ..NONE
    },
    Action {
        name: "wait",
        args: &[
            value("labels"),
            value("pull_request"),
            value("timeout"),
            value("poll"),
        ],
        waiting: true,
        ..NONE
    },
    Action {
        name: "wait_for_ci",
        args: &[
            value("timeout"),
            value("poll"),
            value("grace"),
            value("required"),
        ],
        waiting: true,
        code_host: true,
        ..NONE
    },
];

/// The shortest poll the `wait` action may set.
const MIN_WAIT_ACTION_POLL: Duration = Duration(10 * Duration::SECOND.0);

fn action(name: &str) -> Option<&'static Action> {
    ACTIONS.iter().find(|a| a.name == name)
}

/// The built-in actions, sorted.
pub fn action_names() -> Vec<&'static str> {
    ACTIONS.iter().map(|a| a.name).collect()
}

/// Whether the built-in action runs in the issue's worktree. False for a name that is not
/// a built-in action.
pub fn action_uses_worktree(name: &str) -> bool {
    action(name).is_some_and(|a| a.worktree)
}

/// Whether the stage is an action stage that waits with no session.
pub fn is_waiting_action(stage: &Stage) -> bool {
    stage.kind == KIND_ACTION && action(&stage.action).is_some_and(|a| a.waiting)
}

/// The stage's roles: its own, and its built-in action's default.
pub fn stage_roles(stage: &Stage) -> Vec<String> {
    let mut roles = stage.roles.clone();
    if stage.kind == KIND_ACTION {
        if let Some(role) = action(&stage.action).and_then(|a| a.role) {
            roles.push(role.to_owned());
        }
    }
    roles
}

fn with_text<'a>(stage: &'a Stage, name: &str) -> &'a str {
    stage
        .with
        .as_ref()
        .and_then(|with| with.get(name))
        .map_or("", ActionArg::text)
}

/// Whether the stage cannot run without a code host: an action stage whose built-in
/// action calls one, the `wait` action when it waits on a pull request, or a wait stage
/// that waits on a pull request.
pub fn stage_needs_code_host(stage: &Stage) -> bool {
    match stage.kind.as_str() {
        KIND_WAIT => stage
            .wait
            .as_ref()
            .is_some_and(|w| !w.pull_request.is_empty()),
        KIND_ACTION => {
            (stage.action == "wait" && !with_text(stage, "pull_request").is_empty())
                || action(&stage.action).is_some_and(|a| a.code_host)
        }
        _ => false,
    }
}

/// Whether the stage cannot run without the issue tracker the card's issue lives on.
pub fn stage_needs_issue_host(stage: &Stage) -> bool {
    stage.kind == KIND_ACTION && action(&stage.action).is_some_and(|a| a.issue_host)
}

/// Checks the template variables in `text` against the class of the argument at `path`.
pub fn check_arg_vars(path: &str, text: &str, class: Class) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for found in find_vars(text) {
        let name = found.name;
        if !seen.insert(name) {
            continue;
        }
        // A parameter value is a setting a person typed and the engine checked against
        // the package, so every class may render it. Whether the package declares the
        // name is checked where the package is known.
        if param_variable(name).is_some() {
            continue;
        }
        if !known_variable(name) {
            out.push(Finding::error(
                path,
                "template_unknown_variable",
                format!(
                    "{{{{{name}}}}} is not a template variable; want {}",
                    known_variables_text()
                ),
            ));
        } else if class != Class::Text && !value_variable(name) {
            out.push(Finding::error(
                path,
                "arg_untrusted_variable",
                format!(
                    "{{{{{name}}}}} can carry text from the issue or the agent, so {path} may use only issue.number, issue.url, issue.repo, branch or artifacts.<name>.path"
                ),
            ));
        }
    }
    out
}

/// The findings for an action stage's `action` and `with`. Paths are relative to the stage.
pub fn check_action(stage: &Stage) -> Vec<Finding> {
    use crate::domain::workflow::Workspace;
    if stage.action.is_empty() {
        return vec![Finding::error(
            "action",
            "action_missing",
            format!(
                "an action stage names its action; want one of {}",
                action_names().join(", ")
            ),
        )];
    }
    let Some(spec) = action(&stage.action) else {
        return vec![Finding::error(
            "action",
            "action_unknown",
            format!(
                "{:?} is not an action; want one of {}",
                stage.action,
                action_names().join(", ")
            ),
        )];
    };
    let mut out = Vec::new();
    if stage.workspace == Some(Workspace::None) && spec.worktree {
        out.push(Finding::error(
            "workspace",
            "workspace_not_allowed",
            format!(
                "{} works on the issue's branch, so its workspace cannot be none",
                stage.action
            ),
        ));
    }
    let empty = BTreeMap::new();
    let with = stage.with.as_ref().unwrap_or(&empty);
    for (name, arg) in with {
        let path = format!("with.{name}");
        let Some(arg_spec) = spec.args.iter().find(|a| a.name == name) else {
            let names: Vec<&str> = spec.args.iter().map(|a| a.name).collect();
            let described = if names.is_empty() {
                "none".to_owned()
            } else {
                let mut sorted = names;
                sorted.sort_unstable();
                sorted.join(", ")
            };
            out.push(Finding::error(
                path,
                "arg_unknown",
                format!(
                    "{} takes no argument {name}; it takes {described}",
                    stage.action
                ),
            ));
            continue;
        };
        if arg_spec.class == Class::Argv && !arg.is_list() {
            out.push(Finding::error(
                path,
                "arg_not_list",
                format!("{name} is a list, one entry per argument, such as [npm, test]"),
            ));
            continue;
        }
        if arg_spec.class != Class::Argv && arg.is_list() {
            out.push(Finding::error(
                path,
                "arg_list",
                format!("{name} is a single value, not a list"),
            ));
            continue;
        }
        if arg_spec.class == Class::Argv {
            for (i, element) in arg.list().iter().enumerate() {
                out.extend(check_arg_vars(
                    &format!("{path}[{i}]"),
                    element,
                    Class::Argv,
                ));
            }
            continue;
        }
        out.extend(check_arg_vars(&path, arg.text(), arg_spec.class));
    }
    let mut required_args: Vec<&Arg> = spec.args.iter().filter(|a| a.required).collect();
    required_args.sort_by_key(|a| a.name);
    for arg_spec in required_args {
        let missing = with
            .get(arg_spec.name)
            .is_none_or(|a| a.text().is_empty() && a.list().is_empty());
        if missing {
            out.push(Finding::error(
                format!("with.{}", arg_spec.name),
                "arg_missing",
                format!("{} needs with.{}", stage.action, arg_spec.name),
            ));
        }
    }
    out.extend(extras(&stage.action, with));
    out
}

/// The action that opens a pull request for the card branch.
pub const OPEN_PULL_REQUEST: &str = "open_pull_request";
/// The action that removes the card's worktree.
pub const CLEANUP_WORKTREE: &str = "cleanup_worktree";
/// The action that waits for labels or a pull request state, like a wait stage.
pub const WAIT: &str = "wait";
/// The action that waits for the pull request's checks.
pub const WAIT_FOR_CI: &str = "wait_for_ci";
/// The action that takes the pull request out of draft.
pub const MARK_READY: &str = "mark_ready";
/// The action that merges the pull request.
pub const MERGE: &str = "merge";

type With = BTreeMap<String, ActionArg>;

fn arg_text<'a>(with: &'a With, name: &str) -> &'a str {
    with.get(name).map_or("", ActionArg::text)
}

/// A value with no template variable in it, so it can be checked before the stage runs.
fn literal(arg: &ActionArg) -> bool {
    !has_var(arg.text())
}

/// The checks beyond the argument rules, one set per action.
fn extras(action: &str, with: &With) -> Vec<Finding> {
    match action {
        "open_pull_request" => {
            let mut out = exclusive_args(with, "body", "body_artifact");
            out.extend(bool_arg(with, "draft"));
            out
        }
        "wait_for_ci" => duration_args(with, Duration(0), &["timeout", "poll", "grace"]),
        "merge" => one_of_arg(with, "method", &["merge", "squash", "rebase"]),
        "sync_base" => one_of_arg(with, "method", &["rebase", "merge"]),
        "copy_artifact_to_repo"
        | "check_openspec"
        | "run_command"
        | "resolve_review_thread"
        | "reply_review_comment" => repo_arg(with),
        "wait" => {
            let mut out = one_of_arg(with, "pull_request", &WAIT_PULL_REQUEST_STATES);
            if arg_text(with, "labels").is_empty() && arg_text(with, "pull_request").is_empty() {
                out.push(Finding::error(
                    "with",
                    "wait_labels_missing",
                    "the wait action names labels to wait for in with.labels, or a pull request state in with.pull_request",
                ));
            }
            out.extend(duration_args(
                with,
                MIN_WAIT_ACTION_POLL,
                &["timeout", "poll"],
            ));
            out
        }
        "post_comment" => one_of_args(with, "body", "artifact"),
        "set_labels" => {
            if arg_text(with, "add").is_empty() && arg_text(with, "remove").is_empty() {
                vec![Finding::error(
                    "with",
                    "arg_missing",
                    "set_labels names labels in with.add, with.remove or both",
                )]
            } else {
                Vec::new()
            }
        }
        "create_issue" => {
            let mut out = exclusive_args(with, "body", "body_artifact");
            out.extend(repo_arg(with));
            out
        }
        _ => Vec::new(),
    }
}

/// A literal `with.repo` that is not owner/repo. A value that renders from a variable is
/// checked when the stage runs.
fn repo_arg(with: &With) -> Vec<Finding> {
    match with.get("repo") {
        Some(repo) if !repo.text().is_empty() && literal(repo) && !repo_name(repo.text()) => {
            vec![Finding::error(
                "with.repo",
                "invalid_value",
                format!("with.repo must be owner/repo, got {:?}", repo.text()),
            )]
        }
        _ => Vec::new(),
    }
}

/// Two arguments set together that are alternatives.
fn exclusive_args(with: &With, a: &str, b: &str) -> Vec<Finding> {
    if !arg_text(with, a).is_empty() && !arg_text(with, b).is_empty() {
        vec![Finding::error(
            format!("with.{b}"),
            "arg_conflict",
            format!("set with.{a} or with.{b}, not both"),
        )]
    } else {
        Vec::new()
    }
}

/// Two alternative arguments that are both set, or both left out.
fn one_of_args(with: &With, a: &str, b: &str) -> Vec<Finding> {
    if arg_text(with, a).is_empty() && arg_text(with, b).is_empty() {
        return vec![Finding::error(
            "with",
            "arg_missing",
            format!("set with.{a} or with.{b}"),
        )];
    }
    exclusive_args(with, a, b)
}

/// Go's `strconv.ParseBool`.
fn parse_bool(text: &str) -> bool {
    matches!(
        text,
        "1" | "t" | "T" | "TRUE" | "true" | "True" | "0" | "f" | "F" | "FALSE" | "false" | "False"
    )
}

/// An argument that is not true or false.
fn bool_arg(with: &With, name: &str) -> Vec<Finding> {
    match with.get(name) {
        Some(arg) if literal(arg) && !parse_bool(arg.text()) => vec![Finding::error(
            format!("with.{name}"),
            "invalid_value",
            format!("with.{name} must be true or false, got {:?}", arg.text()),
        )],
        _ => Vec::new(),
    }
}

/// An argument set to a value outside `allowed`.
fn one_of_arg(with: &With, name: &str, allowed: &[&str]) -> Vec<Finding> {
    match with.get(name) {
        Some(arg) if !arg.text().is_empty() && literal(arg) && !allowed.contains(&arg.text()) => {
            vec![Finding::error(
                format!("with.{name}"),
                "invalid_value",
                format!(
                    "with.{name} must be one of {}, got {:?}",
                    allowed.join(", "),
                    arg.text()
                ),
            )]
        }
        _ => Vec::new(),
    }
}

/// A duration argument that does not parse, is not above zero, or, for `poll`, is under
/// `min_poll`.
fn duration_args(with: &With, min_poll: Duration, names: &[&str]) -> Vec<Finding> {
    let mut out = Vec::new();
    for name in names {
        let Some(arg) = with.get(*name) else { continue };
        if arg.text().is_empty() || !literal(arg) {
            continue;
        }
        let path = format!("with.{name}");
        match Duration::parse(arg.text()) {
            Ok(d) if d.nanos() > 0 => {
                if *name == "poll" && d < min_poll {
                    out.push(Finding::error(
                        path,
                        "invalid_value",
                        format!("with.{name} must be at least {min_poll}, got {d}"),
                    ));
                }
            }
            _ => out.push(Finding::error(
                path,
                "invalid_value",
                format!(
                    "with.{name} must be a duration above zero, such as 30s or 2h, got {:?}",
                    arg.text()
                ),
            )),
        }
    }
    out
}
