//! Template variables a stage's text may use: `{{issue.title}}`, `{{params.depth}}`.

use crate::domain::workflow::Stage;
use crate::workflow::load::Package;
use crate::workflow::names::artifact_name;

/// One `{{name}}` in a text: where it starts and ends, and the name between the braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub start: usize,
    pub end: usize,
    pub name: &'a str,
}

/// Every template variable in `text`, in order. A variable is `{{`, optional spaces, a name
/// of letters, digits, `_`, `.` and `-`, optional spaces, then `}}`.
pub fn find_vars(text: &str) -> Vec<Found<'_>> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'{' || bytes[i + 1] != b'{' {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while j < bytes.len() && is_space(bytes[j]) {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && is_name_byte(bytes[j]) {
            j += 1;
        }
        let name_end = j;
        while j < bytes.len() && is_space(bytes[j]) {
            j += 1;
        }
        if name_end > name_start && bytes[j..].starts_with(b"}}") {
            found.push(Found {
                start: i,
                end: j + 2,
                name: &text[name_start..name_end],
            });
            i = j + 2;
        } else {
            i += 1;
        }
    }
    found
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
}

/// Whether `text` holds a template variable.
pub fn has_var(text: &str) -> bool {
    !find_vars(text).is_empty()
}

/// `text` with every variable replaced by `with`.
pub fn replace_vars(text: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in find_vars(text) {
        out.push_str(&text[last..found.start]);
        out.push_str(with);
        last = found.end;
    }
    out.push_str(&text[last..]);
    out
}

/// One template variable a stage's text may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// The variable as written between the braces. A pattern's name holds `<name>` where
    /// an artifact or parameter name goes.
    pub name: String,
    pub description: String,
    /// Marks a variable whose text the engine owns: no issue author, commenter or agent
    /// writes it, so an action argument that becomes a command, a path, a login, a label
    /// or a repo may use it.
    pub value: bool,
    /// Marks `artifacts.<name>.path` and its kin.
    pub pattern: bool,
}

const fn var(
    name: &'static str,
    description: &'static str,
    value: bool,
    pattern: bool,
) -> (&'static str, &'static str, bool, bool) {
    (name, description, value, pattern)
}

/// Every template variable, in the order messages list them. The checks, the renderer and
/// the builder's completion all read this one list.
const VARIABLES: &[(&str, &str, bool, bool)] = &[
    var("issue.title", "The issue's title.", false, false),
    var("issue.body", "The issue's description.", false, false),
    var("issue.url", "The issue's web address.", true, false),
    var("issue.number", "The issue's number.", true, false),
    var(
        "issue.repo",
        "The issue's repository, as owner/repo.",
        true,
        false,
    ),
    var(
        "branch",
        "The card's git branch, buzz/<short id>-<title>.",
        true,
        false,
    ),
    var("scratch", "The run's scratch folder.", false, false),
    var(
        "artifacts.<name>.path",
        "Where an artifact is copied for this stage.",
        true,
        true,
    ),
    var(
        "artifacts.<name>.file",
        "The repo file the skill wrote for an artifact, empty when none is recorded.",
        false,
        true,
    ),
    var(
        "inputs.<name>.file",
        "The repo file of the document bound to this input, empty when none is recorded.",
        false,
        true,
    ),
    var(
        "inputs.<name>.path",
        "Where the document bound to this input is copied for this stage.",
        true,
        true,
    ),
    var("params.<name>", "A package parameter's value.", false, true),
    var(
        "sendback.comments",
        "The comments a person sent the stage back with.",
        false,
        false,
    ),
    var(
        "stage.results",
        "The results this stage may report, comma separated.",
        false,
        false,
    ),
    var(
        "comments",
        "The card's queued comments, quoted.",
        false,
        false,
    ),
    var(
        "repos",
        "The card's repos with each folder and branch. Empty for a one-repo card.",
        false,
        false,
    ),
    var(
        "files",
        "The files linked in the issue, each with its saved path. Empty when there are none.",
        false,
        false,
    ),
];

/// Every variable, as owned values.
pub fn variables() -> Vec<Variable> {
    VARIABLES
        .iter()
        .map(|(name, description, value, pattern)| Variable {
            name: (*name).to_owned(),
            description: (*description).to_owned(),
            value: *value,
            pattern: *pattern,
        })
        .collect()
}

/// The variable names as a message lists them: `a, b or c`.
pub fn known_variables_text() -> String {
    let names: Vec<&str> = VARIABLES.iter().map(|v| v.0).collect();
    let (last, rest) = names.split_last().expect("the variable list is not empty");
    format!("{} or {last}", rest.join(", "))
}

/// Whether `name` is a variable the engine owns, which an action argument that becomes a
/// command, a path, a login, a label or a repo may use.
pub fn value_variable(name: &str) -> bool {
    VARIABLES
        .iter()
        .any(|(n, _, value, pattern)| *value && !*pattern && *n == name)
        || artifact_ref(name, "artifacts.", ".path").is_some()
        || artifact_ref(name, "inputs.", ".path").is_some()
}

/// Whether a prompt may use the template variable `name`.
pub fn known_variable(name: &str) -> bool {
    VARIABLES
        .iter()
        .any(|(n, _, _, pattern)| !*pattern && *n == name)
        || artifact_ref(name, "artifacts.", ".path").is_some()
        || artifact_ref(name, "artifacts.", ".file").is_some()
        || artifact_ref(name, "inputs.", ".path").is_some()
        || artifact_ref(name, "inputs.", ".file").is_some()
}

/// The parameter name in `params.<name>`, when it is a valid parameter name. It does not
/// know which parameters a package declares: the package checks do.
pub fn param_variable(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("params.")?;
    crate::workflow::names::param_name(rest).then_some(rest)
}

/// The artifact (or input) name in `<prefix><name><suffix>` when it is a valid one.
fn artifact_ref<'a>(name: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let rest = name.strip_prefix(prefix)?;
    let art = rest.strip_suffix(suffix)?;
    artifact_name(art).then_some(art)
}

/// The input name in `inputs.<name>.file` or `inputs.<name>.path`.
pub fn input_ref(name: &str) -> Option<&str> {
    artifact_ref(name, "inputs.", ".file").or_else(|| artifact_ref(name, "inputs.", ".path"))
}

/// The variables a stage of `package` may use, as a person writes them: every fixed
/// variable, then `params.<name>` for each declared parameter, then the path and file of
/// each artifact a stage outputs, then the path and file of each input a stage declares.
pub fn draft_variables(package: &Package) -> Vec<Variable> {
    let mut out: Vec<Variable> = variables().into_iter().filter(|v| !v.pattern).collect();
    for parameter in &package.meta.parameters {
        let description = if parameter.description.is_empty() {
            "A package parameter.".to_owned()
        } else {
            parameter.description.clone()
        };
        out.push(Variable {
            name: format!("params.{}", parameter.name),
            description,
            value: false,
            pattern: false,
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for stage in &package.workflow.stages {
        for output in &stage.outputs {
            if !artifact_name(&output.artifact) || !seen.insert(output.artifact.clone()) {
                continue;
            }
            out.push(Variable {
                name: format!("artifacts.{}.path", output.artifact),
                description: format!(
                    "Where the {} artifact is copied for this stage.",
                    output.artifact
                ),
                value: true,
                pattern: false,
            });
            out.push(Variable {
                name: format!("artifacts.{}.file", output.artifact),
                description: format!(
                    "The repo file the skill wrote for the {} artifact.",
                    output.artifact
                ),
                value: false,
                pattern: false,
            });
        }
    }
    seen.clear();
    for stage in &package.workflow.stages {
        for input in &stage.inputs {
            if !artifact_name(&input.name) || !seen.insert(input.name.clone()) {
                continue;
            }
            out.push(Variable {
                name: format!("inputs.{}.path", input.name),
                description: format!("Where the {} input is copied for this stage.", input.name),
                value: true,
                pattern: false,
            });
            out.push(Variable {
                name: format!("inputs.{}.file", input.name),
                description: format!("The repo file of the {} input.", input.name),
                value: false,
                pattern: false,
            });
        }
    }
    out
}

/// In order of first use, each artifact name the stage's prompt text or command arguments
/// mention as `artifacts.<name>.file`. A stage whose text cannot be read names none.
pub fn artifact_names(stage: &Stage, package: Option<&Package>) -> Vec<String> {
    let text = if stage.command.is_empty() && stage.prompt_file.is_empty() {
        stage.prompt.clone()
    } else if let Some(package) = package {
        match package.prompt_text(stage) {
            Ok(text) => text,
            Err(_) => return Vec::new(),
        }
    } else {
        String::new()
    };
    let mut out: Vec<String> = Vec::new();
    for found in find_vars(&text) {
        if let Some(art) = artifact_ref(found.name, "artifacts.", ".file") {
            if !out.iter().any(|a| a == art) {
                out.push(art.to_owned());
            }
        }
    }
    out
}
