//! The workflow file format: `kiln-package.yaml` and `workflow.yaml`.
//!
//! Ported from the Go engine's `core/domain/workflow.go`. One change: a stage's
//! `agent` is the name of a Buzz agent, not a profile with a model and an effort.
//! Those are set on the agent in Buzz.
//!
//! Every type derives `JsonSchema`, and the schema it produces is the one source of
//! truth for the format: the decoder in `crate::workflow::decode` walks a document
//! against it, and the committed `workflow.schema.json` is generated from it.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The file that makes a folder a package.
pub const PACKAGE_FILE: &str = "kiln-package.yaml";
/// A package's stages.
pub const WORKFLOW_FILE: &str = "workflow.yaml";

pub const KIND_PROMPT: &str = "prompt";
pub const KIND_APPROVAL: &str = "approval";
pub const KIND_WAIT: &str = "wait";
pub const KIND_ACTION: &str = "action";
/// Every kind a workflow file can declare, and every kind this build runs.
pub const STAGE_KINDS: [&str; 4] = [KIND_PROMPT, KIND_APPROVAL, KIND_WAIT, KIND_ACTION];

pub const ROLE_OPENS_PULL_REQUEST: &str = "opens_pull_request";
pub const ROLE_MERGES: &str = "merges";
pub const ROLE_CLOSES_ISSUE: &str = "closes_issue";
/// What a stage can do to an issue or its pull request.
pub const ROLES: [&str; 3] = [ROLE_OPENS_PULL_REQUEST, ROLE_MERGES, ROLE_CLOSES_ISSUE];

pub const RESULT_PASSED: &str = "passed";
pub const RESULT_NEEDS_WORK: &str = "needs_work";
pub const RESULT_FAILED: &str = "failed";
pub const RESULT_NEEDS_INPUT: &str = "needs_input";
/// The results every prompt stage may report.
pub const STANDARD_RESULTS: [&str; 4] = [
    RESULT_PASSED,
    RESULT_NEEDS_WORK,
    RESULT_FAILED,
    RESULT_NEEDS_INPUT,
];

/// The pull request states a wait can wait for.
pub const WAIT_PULL_REQUEST_STATES: [&str; 2] = ["approved", "merged"];

/// Metadata in a package's `kiln-package.yaml`. The package name is the workflow's identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PackageFile {
    pub name: String,
    pub version: String,
    pub description: String,
    /// The typed values the package asks for. Each becomes the setting `params.<name>`.
    pub parameters: Vec<Parameter>,
}

/// What kind of value a package parameter holds. Every value is stored and passed as
/// a string; the type decides which strings are valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    Text,
    Number,
    Bool,
    Choice,
    Branch,
}

impl ParamType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Choice => "choice",
            Self::Branch => "branch",
        }
    }
}

/// One typed parameter a package declares. `default` is always text, even for a number.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Parameter {
    pub name: String,
    /// `None` when the file leaves it out or names a type that does not exist. The
    /// decoder reports the second case.
    #[serde(rename = "type")]
    pub kind: Option<ParamType>,
    pub default: String,
    pub description: String,
    pub choices: Vec<String>,
}

/// A package's `workflow.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowFile {
    pub version: i64,
    pub stages: Vec<Stage>,
}

/// Whether a stage runs in the issue's worktree. `none` starts it with no worktree,
/// and `branch` prepares the worktree on the issue's branch first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Workspace {
    None,
    Branch,
}

/// One stage of a workflow. Routes, results and every behavior key off `kind` and the
/// declared results, never off `id` or `name`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Stage {
    pub id: String,
    pub name: String,
    pub description: String,
    /// One of prompt, approval, wait or action. Left open here so a misspelling is the
    /// finding `kind_unknown` and not a decode error.
    #[schemars(extend("x-known-values" = ["prompt", "approval", "wait", "action"]))]
    pub kind: String,
    /// The name of the Buzz agent that works the stage. Its profile, model and effort
    /// are set on the agent in Buzz.
    pub agent: Option<String>,
    pub prompt: String,
    pub prompt_file: String,
    /// The bare name of a plugin command to run, with no package prefix.
    pub command: String,
    /// The argument line sent after the command. It holds `{{variables}}`.
    pub args: String,
    /// The documents a prompt stage reads. A stage with inputs reads only these plus its own outputs.
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    /// The stage's own results, beside the standard ones.
    pub results: Vec<String>,
    /// Result to route. A route is a stage id, `stop`, `ask`, or a call that gives the target other documents.
    pub on: BTreeMap<String, Route>,
    /// Backward routes in a row from this stage before a person is asked.
    pub loop_limit: i64,
    /// Hold the card before this stage until a person starts it.
    pub pause: bool,
    pub execution: Execution,
    pub repeat: Option<Repeat>,
    pub conditions: Option<Conditions>,
    pub approval: Option<ApprovalSettings>,
    pub wait: Option<Wait>,
    /// The built-in action an action stage runs.
    pub action: String,
    /// The action's arguments by name. `Some` with no entries is not the same as `None`:
    /// a prompt stage that writes `with: {}` still gets `with_not_allowed`.
    pub with: Option<BTreeMap<String, ActionArg>>,
    /// What the stage does to the issue or its pull request, beside the default role of
    /// its built-in action. One of opens_pull_request, merges or closes_issue.
    #[schemars(extend("x-known-values" = ["opens_pull_request", "merges", "closes_issue"]))]
    pub roles: Vec<String>,
    /// Empty derives it from the stage's kind and action.
    pub workspace: Option<Workspace>,
}

/// A document a prompt stage reads. `required` means the stage cannot start until the
/// document has a saved version.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Input {
    pub name: String,
    pub required: bool,
}

/// An artifact a stage produces.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Output {
    pub artifact: String,
    pub handoff: Option<bool>,
    /// The repo file the stage writes, as a path pattern with `{{variables}}` and `*`.
    pub file: String,
    /// The stage may pass without saving this document, so a missing version opens no question.
    pub optional: bool,
}

/// A stage's timeouts and retries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Execution {
    pub timeout: Duration,
    pub retries: Retries,
}

/// Reruns a failed stage before its failed route.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Retries {
    pub max: i64,
    pub backoff: Duration,
}

/// Runs a stage in rounds until its plan artifact is complete.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Repeat {
    pub until: String,
    pub plan: String,
    pub max_rounds: i64,
    pub stall_after: i64,
    pub fresh_session: bool,
}

/// Checked before a stage opens a session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Conditions {
    pub labels: Vec<String>,
    #[serde(rename = "else")]
    pub else_to: String,
}

/// The artifacts an approval stage asks a person to approve.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalSettings {
    pub artifacts: Vec<String>,
}

/// What a wait stage waits for: labels on the issue, a state of its pull request, or
/// both, and its timing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Wait {
    pub labels: Vec<String>,
    pub pull_request: String,
    pub poll: Duration,
    pub timeout: Duration,
}

/// One argument of an action stage: a string, or for an argument that takes one, a
/// list of strings such as `run_command`'s argv.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ActionArg {
    Text(String),
    List(Vec<String>),
}

impl ActionArg {
    /// The text of a value, and `""` for a list.
    pub fn text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::List(_) => "",
        }
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Self::List(_))
    }

    /// The elements of a list, and nothing for a value.
    pub fn list(&self) -> &[String] {
        match self {
            Self::Text(_) => &[],
            Self::List(list) => list,
        }
    }
}

/// How a route reads in a file: a stage id, or a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RouteForm {
    Id(String),
    Call(RouteCall),
}

/// A call: the target, the documents it is given, and where the card goes after it passes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RouteCall {
    pub to: String,
    /// Maps one of the target's input names to the artifact it reads this time.
    pub give: BTreeMap<String, String>,
    pub then: String,
}

/// Where a result sends the card. It reads as a stage id, `stop` or `ask`, or as a call
/// that also gives the target different documents.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "RouteForm", into = "RouteForm")]
#[schemars(with = "RouteForm")]
pub struct Route {
    pub to: String,
    pub give: BTreeMap<String, String>,
    pub then: String,
}

impl Route {
    /// Whether the route carries more than a target.
    pub fn is_call(&self) -> bool {
        !self.give.is_empty() || !self.then.is_empty()
    }
}

impl From<RouteForm> for Route {
    fn from(form: RouteForm) -> Self {
        match form {
            RouteForm::Id(to) => Route {
                to,
                ..Route::default()
            },
            RouteForm::Call(call) => Route {
                to: call.to,
                give: call.give,
                then: call.then,
            },
        }
    }
}

impl From<Route> for RouteForm {
    fn from(route: Route) -> Self {
        if route.is_call() {
            RouteForm::Call(RouteCall {
                to: route.to,
                give: route.give,
                then: route.then,
            })
        } else {
            RouteForm::Id(route.to)
        }
    }
}

/// How serious a check finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    /// Something worth knowing that is not a problem.
    Note,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }
}

/// One problem the workflow checks found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub path: String,
    pub code: String,
    pub severity: Severity,
    pub message: String,
}

impl Finding {
    pub fn error(path: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code: code.to_owned(),
            severity: Severity::Error,
            message: message.into(),
        }
    }

    pub fn warning(path: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(path, code, message)
        }
    }
}

/// A length of time written as a Go duration string such as `30m` or `1h30m`, held as
/// nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Duration(pub i64);

impl Duration {
    pub const SECOND: Duration = Duration(1_000_000_000);

    pub fn nanos(self) -> i64 {
        self.0
    }

    /// Parses a Go duration: an optional sign, then one or more numbers (decimals allowed)
    /// each followed by a unit of ns, us, µs, ms, s, m or h. A bare `0` is allowed.
    pub fn parse(text: &str) -> Result<Self, String> {
        let original = text;
        let (negative, mut rest) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        if rest == "0" {
            return Ok(Duration(0));
        }
        if rest.is_empty() {
            return Err(format!("invalid duration {original:?}"));
        }
        let mut total: i128 = 0;
        while !rest.is_empty() {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            let whole = &rest[..digits];
            rest = &rest[digits..];
            let mut fraction = "";
            if let Some(after) = rest.strip_prefix('.') {
                let n = after.bytes().take_while(u8::is_ascii_digit).count();
                fraction = &after[..n];
                rest = &after[n..];
            }
            if whole.is_empty() && fraction.is_empty() {
                return Err(format!("invalid duration {original:?}"));
            }
            let unit_len = rest
                .find(|c: char| c == '.' || c.is_ascii_digit())
                .unwrap_or(rest.len());
            let unit = &rest[..unit_len];
            rest = &rest[unit_len..];
            if unit.is_empty() {
                return Err(format!("missing unit in duration {original:?}"));
            }
            let scale: i128 = match unit {
                "ns" => 1,
                "us" | "\u{b5}s" | "\u{3bc}s" => 1_000,
                "ms" => 1_000_000,
                "s" => 1_000_000_000,
                "m" => 60_000_000_000,
                "h" => 3_600_000_000_000,
                _ => return Err(format!("unknown unit {unit:?} in duration {original:?}")),
            };
            let overflow = || format!("invalid duration {original:?}");
            let whole_value: i128 = if whole.is_empty() {
                0
            } else {
                whole.parse().map_err(|_| overflow())?
            };
            total = whole_value
                .checked_mul(scale)
                .and_then(|v| total.checked_add(v))
                .ok_or_else(overflow)?;
            if !fraction.is_empty() {
                // Digits beyond the 18th change nothing a nanosecond can show.
                let kept = &fraction[..fraction.len().min(18)];
                let numerator: i128 = kept.parse().map_err(|_| overflow())?;
                let denominator = 10_i128.pow(kept.len() as u32);
                total = total
                    .checked_add(numerator * scale / denominator)
                    .ok_or_else(overflow)?;
            }
            if total > i64::MAX as i128 {
                return Err(overflow());
            }
        }
        let signed = if negative { -total } else { total };
        Ok(Duration(signed as i64))
    }
}

impl fmt::Display for Duration {
    /// Go's `Duration.String`: `0s`, `1.5s`, `30m0s`, `1h30m0s`, `250ms`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut value = self.0.unsigned_abs();
        if self.0 == 0 {
            return f.write_str("0s");
        }
        let sign = if self.0 < 0 { "-" } else { "" };
        if value < 1_000_000_000 {
            let (unit, scale) = if value < 1_000 {
                ("ns", 1)
            } else if value < 1_000_000 {
                ("\u{b5}s", 1_000)
            } else {
                ("ms", 1_000_000)
            };
            return write!(f, "{sign}{}{unit}", trim_fraction(value, scale));
        }
        let hours = value / 3_600_000_000_000;
        value %= 3_600_000_000_000;
        let minutes = value / 60_000_000_000;
        value %= 60_000_000_000;
        let seconds = trim_fraction(value, 1_000_000_000);
        write!(f, "{sign}")?;
        if hours > 0 {
            write!(f, "{hours}h")?;
        }
        if hours > 0 || minutes > 0 {
            write!(f, "{minutes}m")?;
        }
        write!(f, "{seconds}s")
    }
}

/// `value / scale` as a decimal with no trailing zeros.
fn trim_fraction(value: u64, scale: u64) -> String {
    let whole = value / scale;
    let rest = value % scale;
    if rest == 0 {
        return whole.to_string();
    }
    let width = scale.ilog10() as usize;
    let fraction = format!("{rest:0width$}");
    format!("{whole}.{}", fraction.trim_end_matches('0'))
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Duration::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Duration {
    fn schema_name() -> Cow<'static, str> {
        "Duration".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "go-duration",
            "description": "a Go duration, such as 30m or 1h30m"
        })
    }
}
