//! The message that asks a stage's agent to do the stage.
//!
//! Ported from the Go engine's `RenderPrompt` (`core/workflow/render.go`). The variables
//! that need an issue, a worktree or a scratch folder (`issue.url`, `branch`, `scratch`,
//! `artifacts.*`, `inputs.*`) render as empty text. A stage reads its artifacts through the
//! input and output sections this message adds and the `read_artifact` and `save_artifact`
//! tools.

use std::collections::BTreeMap;

use crate::domain::card::card_branch;
use crate::domain::workflow::{Stage, Workspace, KIND_PROMPT};
use crate::tool_names::{READ_ARTIFACT, SAVE_ARTIFACT, STAGE_DONE, UPDATE_PLAN};
use crate::workflow::load::Package;
use crate::workflow::routes::reportable_results;
use crate::workflow::template::{find_vars, known_variable, param_variable, variables};

/// What the message says about the card.
pub struct CardText<'a> {
    /// The card's id, which the agent passes back to `stage_done`.
    pub id: uuid::Uuid,
    pub title: &'a str,
    pub description: &'a str,
}

/// The card's issue, for the variables that name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueText {
    /// The repo as `owner/name`.
    pub repo: String,
    pub number: u64,
    pub url: String,
}

/// A text input under this many bytes is written into the task. A larger one is named, and
/// the agent reads it with `read_artifact`.
pub const INLINE_LIMIT: usize = 8 * 1024;

/// The most bytes the relay takes in one message.
pub const MAX_POST_BYTES: usize = 64 * 1024;

/// Bytes [`render_task`] leaves free for what is added after it: the card link every first
/// post of a thread ends with, and a round's line.
const TAIL_RESERVE: usize = 2 * 1024;

/// The most bytes [`render_task`] writes.
pub const TASK_BUDGET: usize = MAX_POST_BYTES - TAIL_RESERVE;

/// Ends a description cut short to fit the message.
pub const DESCRIPTION_CUT: &str =
    "\n\n(description cut here to fit the message; the whole text is on the card)";

/// What the stage's agent is told about one artifact it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputView {
    pub name: String,
    pub state: InputState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputState {
    /// The stage declares the input and no version is saved yet.
    Missing,
    Text {
        version: i32,
        bytes: usize,
        /// The text itself, when it is under [`INLINE_LIMIT`].
        body: Option<String>,
    },
    File {
        version: i32,
        size: i64,
        media_type: String,
        /// What `buzz media get` takes: the blob's URL, else its hash.
        media_ref: String,
    },
}

/// What the task says beyond the card and the stage's prompt.
#[derive(Debug, Clone, Default)]
pub struct Handoff {
    /// The artifacts the stage reads.
    pub inputs: Vec<InputView>,
    /// What earlier sessions on this stage left, when it has been visited before.
    pub brief: String,
    /// The channel the task is posted in, which a file is posted in too.
    pub channel: Option<uuid::Uuid>,
    /// The visit the task opens. The agent passes it back with the stage's id, so a report
    /// for an earlier visit cannot move the card on from this one.
    pub visit: u32,
    /// The comments a person sent the card back with from an approval stage, empty when the
    /// card did not come back to this stage.
    pub sendback: String,
    /// The card's issue, when it has one. The card's work is on its branch in that repo, so a
    /// stage that edits code is told to push it.
    pub issue: Option<IssueText>,
    /// The card's repos, first repo first. The card runs in the first; the others are context.
    pub repos: Vec<String>,
}

/// The id buzz-app registers Beehive under (`PLUGIN_ID` in `src/features/beehive/routes.ts`).
pub const BEEHIVE_FEATURE_ID: &str = "buzz.beehive";
/// The id of the page Beehive registers (`PAGE_ID` in the same file).
pub const BEEHIVE_PAGE_ID: &str = "beehive";

/// Link a Buzz client opens as the card's page. buzz-app's link parser turns it into a plugin
/// link, and the page's own route check accepts the card id (`parsePluginLink` in
/// `src/features/navigation/buzz-links.ts`), so it opens inside the app and not at the
/// service's address.
pub fn card_link(card: uuid::Uuid) -> String {
    format!("buzz://plugin?id={BEEHIVE_FEATURE_ID}&page={BEEHIVE_PAGE_ID}&route.card={card}")
}

/// `text` followed by a line that links back to the card, for the posts a person opens the
/// card from: the status post and the first post of a stage thread.
pub fn with_card_link(text: &str, card: uuid::Uuid) -> String {
    format!("{text}\n\nOpen the card in Beehive: {}", card_link(card))
}

/// The message for `stage`: the card's title and description, the stage's prompt with its
/// variables filled in, and the instruction to report a result. A stage that runs a plugin
/// command starts with the command line instead, because the agent reads a leading slash as
/// the command, and the card follows it. `params` are the resolved `params.<name>` values.
///
/// The message stays within [`TASK_BUDGET`] bytes, so the instruction to report a result, which
/// ends it, always reaches the agent. A description too long for that is cut, wherever it is
/// written, and ends with [`DESCRIPTION_CUT`]. Everything else is written whole.
pub fn render_task(
    package: &Package,
    stage: &Stage,
    card: &CardText<'_>,
    params: &BTreeMap<String, String>,
    handoff: &Handoff,
) -> Result<String, String> {
    let render =
        |description: &str| render_with(package, stage, card, description, params, handoff);
    let full = render(card.description)?;
    if full.len() <= TASK_BUDGET || card.description.is_empty() {
        return Ok(full);
    }
    // What is left once the description is gone is what the cut one must fit in. The
    // description may be written more than once, and a quote in a command's arguments grows
    // when it is escaped, so the first guess is corrected until the message fits.
    let base = render("")?;
    let copies = (full.len() - base.len())
        .div_ceil(card.description.len())
        .max(1);
    let mut allow = TASK_BUDGET.saturating_sub(base.len()) / copies;
    loop {
        let text = render(&cut_description(card.description, allow))?;
        if text.len() <= TASK_BUDGET || allow == 0 {
            return Ok(text);
        }
        allow -= (text.len() - TASK_BUDGET).div_ceil(copies).clamp(1, allow);
    }
}

/// `description` as it is, or cut at a character boundary so that it and [`DESCRIPTION_CUT`]
/// take at most `allow` bytes. Nothing is left when there is not room for the marker.
fn cut_description(description: &str, allow: usize) -> String {
    if description.len() <= allow {
        return description.to_owned();
    }
    let Some(mut end) = allow.checked_sub(DESCRIPTION_CUT.len()).filter(|n| *n > 0) else {
        return String::new();
    };
    while !description.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{DESCRIPTION_CUT}", &description[..end])
}

fn render_with(
    package: &Package,
    stage: &Stage,
    card: &CardText<'_>,
    description: &str,
    params: &BTreeMap<String, String>,
    handoff: &Handoff,
) -> Result<String, String> {
    let text = package.prompt_text(stage)?;
    let results = reportable_results(stage);
    let mut values = base_values(card, description, params, handoff.issue.as_ref());
    values.insert("stage.results".into(), results.join(", "));
    values.insert("sendback.comments".into(), handoff.sendback.clone());
    let card_block = match (card.title.is_empty(), description.is_empty()) {
        (_, true) => card.title.to_owned(),
        (true, false) => description.to_owned(),
        (false, false) => format!("{}\n\n{description}", card.title),
    };
    let body = if stage.command.is_empty() {
        render_template(&text, &values)
            .trim_end_matches('\n')
            .to_owned()
    } else {
        // `prompt_text` gave `/prefix:command args`.
        let (command, args) = text.split_once(' ').unwrap_or((text.as_str(), ""));
        let mut line = command.to_owned();
        if !args.is_empty() {
            line.push(' ');
            line.push_str(&render_args(args, &values));
        }
        line.trim_end_matches('\n').to_owned()
    };
    let mut parts = Vec::new();
    if stage.command.is_empty() {
        parts.push(card_block);
        parts.push(body);
    } else {
        parts.push(body);
        parts.push(card_block);
    }
    // The comments reach the agent even when the prompt does not ask for them.
    let asked = find_vars(&text)
        .iter()
        .any(|found| found.name == "sendback.comments");
    if !asked && !handoff.sendback.is_empty() {
        parts.push(format!(
            "A person sent the card back with these comments:\n{}",
            handoff.sendback
        ));
    }
    parts.push(handoff.brief.clone());
    parts.push(inputs_section(&handoff.inputs));
    parts.push(outputs_section(stage, card.id, handoff.channel));
    parts.push(repos_section(&handoff.repos, handoff.issue.is_some()));
    if (handoff.issue.is_some() || !handoff.repos.is_empty()) && edits_code(stage) {
        parts.push(push_line(&card_branch(card.id, card.title)));
    }
    parts.push(done_line(card.id, &stage.id, handoff.visit, &results));
    Ok(parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
}

/// The variables every text of a card renders: the issue and the card branch, empty where the
/// card has no issue, and the run's parameters.
pub fn base_values(
    card: &CardText<'_>,
    description: &str,
    params: &BTreeMap<String, String>,
    issue: Option<&IssueText>,
) -> BTreeMap<String, String> {
    let mut values: BTreeMap<String, String> = variables()
        .into_iter()
        .filter(|v| !v.pattern)
        .map(|v| (v.name, String::new()))
        .collect();
    values.insert("issue.title".into(), card.title.to_owned());
    values.insert("issue.body".into(), description.to_owned());
    values.insert("branch".into(), card_branch(card.id, card.title));
    if let Some(issue) = issue {
        values.insert("issue.repo".into(), issue.repo.clone());
        values.insert("issue.number".into(), issue.number.to_string());
        values.insert("issue.url".into(), issue.url.clone());
    }
    for (name, value) in params {
        values.insert(format!("params.{name}"), value.clone());
    }
    values
}

/// Which repo the card's work is in, and which others are context. Empty for a card whose one
/// repo is its issue's, which the other lines already name.
fn repos_section(repos: &[String], has_issue: bool) -> String {
    let Some((first, rest)) = repos.split_first() else {
        return String::new();
    };
    if rest.is_empty() && has_issue {
        return String::new();
    }
    let mut text = format!("Work in the repo `{first}`.");
    if !rest.is_empty() {
        let linked: Vec<String> = rest.iter().map(|repo| format!("`{repo}`")).collect();
        text.push_str(&format!(
            " The card also links {}, which is context to read, not a place to change.",
            linked.join(", ")
        ));
    }
    text
}

/// Whether the stage's agent works in the card's checkout: a prompt stage whose workspace is
/// not `none` (the check refuses that for a prompt stage, so it is the default).
fn edits_code(stage: &Stage) -> bool {
    stage.kind == KIND_PROMPT && stage.workspace != Some(Workspace::None)
}

/// What the agent is told about handing its changes on. The next stage may be another agent on
/// another machine, and gets the work from git alone.
fn push_line(branch: &str) -> String {
    format!(
        "If you changed files in the repository, commit them with a clear message and push the branch `{branch}` before you call `{STAGE_DONE}`. The next stage may be a different agent, and gets your work only from git."
    )
}

/// The instruction to report a result: which tool, and the card, stage and visit it takes.
pub fn done_line(card: uuid::Uuid, stage: &str, visit: u32, results: &[String]) -> String {
    format!(
        "When you are done, call the Beehive tool `{STAGE_DONE}` with card_id `{card}`, stage `{stage}`, visit {visit} and result one of: {}.",
        results.join(", ")
    )
}

/// The longest run of backticks in `text`, so a fence can be longer.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

fn human_bytes(bytes: usize) -> String {
    if bytes >= 1024 {
        format!("{} KiB", bytes.div_ceil(1024))
    } else {
        format!("{bytes} bytes")
    }
}

/// The artifacts the stage reads, by name, with small documents written out.
fn inputs_section(inputs: &[InputView]) -> String {
    if inputs.is_empty() {
        return String::new();
    }
    let mut lines = vec!["Input artifacts:".to_owned()];
    for input in inputs {
        let name = &input.name;
        lines.push(match &input.state {
            InputState::Missing => format!("- `{name}`: not saved yet."),
            InputState::Text {
                version,
                bytes,
                body: Some(body),
            } => {
                let fence = fence_for(body);
                format!(
                    "- `{name}` (text, version {version}, {}):\n{fence}\n{body}\n{fence}",
                    human_bytes(*bytes)
                )
            }
            InputState::Text { version, bytes, .. } => format!(
                "- `{name}` (text, version {version}, {}): too long to write here. Read it with the Beehive tool `{READ_ARTIFACT}`.",
                human_bytes(*bytes)
            ),
            InputState::File {
                version,
                size,
                media_type,
                media_ref,
            } => format!(
                "- `{name}` (file, version {version}, {media_type}, {size} bytes): download it with `buzz media get {media_ref} -o <path>`."
            ),
        });
    }
    lines.join("\n")
}

/// The artifacts the stage must or may save, and how to save a document or a file.
fn outputs_section(stage: &Stage, card: uuid::Uuid, channel: Option<uuid::Uuid>) -> String {
    let plan = stage.repeat.as_ref().map(|r| r.plan.as_str()).unwrap_or("");
    if stage.outputs.is_empty() && plan.is_empty() {
        return String::new();
    }
    let mut lines = vec!["Outputs to save before you report a result:".to_owned()];
    for output in &stage.outputs {
        let optional = if output.optional { " (optional)" } else { "" };
        lines.push(format!("- `{}`{optional}", output.artifact));
    }
    if !plan.is_empty() {
        lines.push(format!(
            "- `{plan}`: the plan. Save it as a Markdown checklist, then check items off with the Beehive tool `{UPDATE_PLAN}`."
        ));
    }
    lines.push(format!(
        "Save a document with the Beehive tool `{SAVE_ARTIFACT}`, with card_id `{card}`, the artifact's name and its full text as content. Each call saves a new version."
    ));
    let channel = channel.map_or_else(|| "<channel>".to_owned(), |c| c.to_string());
    lines.push(format!(
        "To save a file, such as an image or a PDF, upload it with `buzz upload file --file <path>`, post the file in your own thread with `buzz messages send --channel {channel} --reply-to <the id of the message that mentioned you> --file <path> --content <name>`, then call `{SAVE_ARTIFACT}` with card_id `{card}`, the artifact's name, kind `file`, and the sha256, size and type the upload printed."
    ));
    lines.join("\n")
}

/// What the variable `name` renders as. An unknown variable stays as written.
fn variable_value(written: &str, name: &str, values: &BTreeMap<String, String>) -> String {
    if let Some(value) = values.get(name) {
        return value.clone();
    }
    if param_variable(name).is_some() || known_variable(name) {
        return String::new();
    }
    written.to_owned()
}

/// `text` with each template variable replaced by its value.
pub fn render_template(text: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in find_vars(text) {
        out.push_str(&text[last..found.start]);
        out.push_str(&variable_value(
            &text[found.start..found.end],
            found.name,
            values,
        ));
        last = found.end;
    }
    out.push_str(&text[last..]);
    out
}

/// [`render_template`] for a command's arguments. A variable alone inside single quotes is
/// rendered so a shell-style reader gets its value back whole: each single quote in the
/// value is closed, escaped and opened again. One alone inside double quotes gets each
/// backslash and double quote escaped.
pub fn render_args(args: &str, values: &BTreeMap<String, String>) -> String {
    let bytes = args.as_bytes();
    let mut out = String::with_capacity(args.len());
    let mut last = 0;
    for found in find_vars(args) {
        out.push_str(&args[last..found.start]);
        last = found.end;
        let mut value = variable_value(&args[found.start..found.end], found.name, values);
        let quote = (found.start > 0
            && found.end < bytes.len()
            && bytes[found.start - 1] == bytes[found.end])
            .then(|| bytes[found.end]);
        match quote {
            Some(b'\'') => value = value.replace('\'', r"'\''"),
            Some(b'"') => value = value.replace('\\', r"\\").replace('"', r#"\""#),
            _ => {}
        }
        out.push_str(&value);
    }
    out.push_str(&args[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::workflow::{PackageFile, WorkflowFile, KIND_PROMPT};

    fn package(files: &[(&str, &str)]) -> Package {
        Package {
            meta: PackageFile {
                name: "demo".into(),
                ..PackageFile::default()
            },
            workflow: WorkflowFile::default(),
            path: "test/demo".into(),
            files: files
                .iter()
                .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
                .collect(),
            findings: Vec::new(),
            builtin: false,
        }
    }

    fn stage(prompt: &str) -> Stage {
        Stage {
            id: "fix".into(),
            kind: KIND_PROMPT.into(),
            prompt: prompt.into(),
            ..Stage::default()
        }
    }

    const CARD: uuid::Uuid = uuid::Uuid::from_u128(0x2a);

    fn card() -> CardText<'static> {
        CardText {
            id: CARD,
            title: "the bug",
            description: "It breaks.",
        }
    }

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_prompt_renders_the_card_variables_and_ends_with_the_report_instruction() {
        let mut s = stage("Fix {{ issue.title }}.\n{{issue.body}}\nPick {{stage.results}}.");
        s.results = vec!["blocked".into()];
        let got = render_task(
            &package(&[]),
            &s,
            &card(),
            &params(&[]),
            &Handoff {
                visit: 3,
                ..Handoff::default()
            },
        )
        .unwrap();
        // `needs_work` has no route here, so the agent is not offered it.
        let want = "the bug\n\nIt breaks.\n\n\
            Fix the bug.\nIt breaks.\nPick passed, failed, needs_input, blocked.\n\n\
            When you are done, call the Beehive tool `stage_done` with card_id `00000000-0000-0000-0000-00000000002a`, stage `fix`, visit 3 and result one of: passed, failed, needs_input, blocked.";
        assert_eq!(got, want);
    }

    /// A card whose description is `unit` repeated to the most characters a card takes.
    fn long_card(unit: &str) -> (String, CardText<'static>) {
        let description: &'static str = Box::leak(unit.repeat(65_536).into_boxed_str());
        (
            description.to_owned(),
            CardText {
                id: CARD,
                title: "the bug",
                description,
            },
        )
    }

    #[test]
    fn a_long_description_is_cut_so_the_report_instruction_still_ends_the_task() {
        // One byte, two bytes and four bytes a character, each at the most the card takes.
        for unit in ["x", "\u{e9}", "\u{1f41d}"] {
            let (description, card) = long_card(unit);
            let handoff = Handoff {
                visit: 2,
                ..Handoff::default()
            };
            let s = stage("Fix {{issue.title}}.");
            let got = render_task(&package(&[]), &s, &card, &params(&[]), &handoff).unwrap();
            assert!(got.len() <= TASK_BUDGET, "{unit}: {} bytes", got.len());
            let done = done_line(CARD, "fix", 2, &reportable_results(&s));
            assert!(got.ends_with(&done), "{unit}: the instruction was cut");
            assert!(got.contains(DESCRIPTION_CUT), "{unit}: no marker");
            // The cut is made at a character, and keeps the start of the description.
            let kept = got
                .split(DESCRIPTION_CUT)
                .next()
                .unwrap()
                .strip_prefix("the bug\n\n")
                .unwrap();
            assert!(description.starts_with(kept) && !kept.is_empty(), "{unit}");
            // With the link and a round's line added after it, the post still fits whole.
            let post = format!(
                "{}\n\n{}",
                with_card_link(&got, CARD),
                "x".repeat(TAIL_RESERVE - 200)
            );
            assert!(post.len() <= MAX_POST_BYTES, "{unit}: {} bytes", post.len());
        }
    }

    #[test]
    fn a_description_written_twice_or_in_a_command_still_leaves_room_for_the_instruction() {
        let (_, card) = long_card("'");
        // In the prompt too, and the quotes grow when a command's arguments escape them.
        let prompt = stage("A {{issue.body}} B {{issue.body}}");
        let mut command = stage("");
        command.command = "review".into();
        command.args = "'{{issue.body}}'".into();
        for s in [prompt, command] {
            let got =
                render_task(&package(&[]), &s, &card, &params(&[]), &Handoff::default()).unwrap();
            assert!(got.len() <= TASK_BUDGET, "{} bytes", got.len());
            assert!(
                got.ends_with(&done_line(CARD, "fix", 0, &reportable_results(&s))),
                "the instruction was cut"
            );
        }
    }

    #[test]
    fn a_description_that_fits_is_written_whole() {
        let description = "d".repeat(TASK_BUDGET - 1_000);
        let card = CardText {
            id: CARD,
            title: "the bug",
            description: &description,
        };
        let got = render_task(
            &package(&[]),
            &stage("Fix."),
            &card,
            &params(&[]),
            &Handoff::default(),
        )
        .unwrap();
        assert!(got.contains(&description) && !got.contains(DESCRIPTION_CUT));
    }

    #[test]
    fn a_param_renders_its_value_and_a_missing_one_renders_empty() {
        let s = stage("Depth {{params.depth}}, area [{{params.area}}], tone {{ params.tone }}.");
        let got = render_task(
            &package(&[]),
            &s,
            &card(),
            &params(&[("depth", "deep"), ("tone", "")]),
            &Handoff::default(),
        )
        .unwrap();
        assert!(
            got.contains("Depth deep, area [], tone ."),
            "rendered task:\n{got}"
        );
    }

    #[test]
    fn an_unknown_variable_stays_as_written_and_a_bad_param_name_is_not_a_param() {
        let got = render_template(
            "{{issue.titel}} {{params.Bad}} {{params.x}}",
            &BTreeMap::new(),
        );
        assert_eq!(got, "{{issue.titel}} {{params.Bad}} ");
    }

    #[test]
    fn a_command_renders_as_a_slash_line_and_the_card_follows_it() {
        let mut s = stage("");
        s.prompt = String::new();
        s.command = "review".into();
        s.args = "title {{issue.title}}".into();
        let got = render_task(
            &package(&[]),
            &s,
            &card(),
            &params(&[]),
            &Handoff::default(),
        )
        .unwrap();
        assert!(
            got.starts_with("/demo:review title the bug\n\nthe bug\n\nIt breaks.\n\n"),
            "rendered task:\n{got}"
        );
        assert!(got.ends_with(
            "`stage_done` with card_id `00000000-0000-0000-0000-00000000002a`, stage `fix`, visit 0 and result one of: passed, failed, needs_input."
        ));
    }

    #[test]
    fn a_command_with_no_args_is_the_bare_command() {
        let mut s = stage("");
        s.command = "review".into();
        let got = render_task(
            &package(&[]),
            &s,
            &card(),
            &params(&[]),
            &Handoff::default(),
        )
        .unwrap();
        assert!(got.starts_with("/demo:review\n\nthe bug"), "{got}");
    }

    #[test]
    fn a_prompt_file_is_read_from_the_package() {
        let mut s = stage("");
        s.prompt_file = "prompts/p.md".into();
        let pkg = package(&[("prompts/p.md", "Look at {{issue.title}}\n")]);
        let got = render_task(&pkg, &s, &card(), &params(&[]), &Handoff::default()).unwrap();
        assert!(
            got.contains("\n\nLook at the bug\n\nWhen you are done"),
            "{got}"
        );
        let mut missing = stage("");
        missing.prompt_file = "prompts/none.md".into();
        assert!(render_task(&pkg, &missing, &card(), &params(&[]), &Handoff::default()).is_err());
    }

    #[test]
    fn a_variable_in_quotes_in_command_args_is_escaped_to_hold_whole() {
        let values = params(&[("issue.title", "it's \"x\""), ("params.a", "b c")]);
        assert_eq!(
            render_args("'{{issue.title}}' {{params.a}}", &values),
            r#"'it'\''s "x"' b c"#
        );
        assert_eq!(
            render_args("\"{{issue.title}}\"", &values),
            r#""it's \"x\"""#
        );
        // An unbound input renders empty and keeps its quotes.
        assert_eq!(render_args("'{{inputs.plan.file}}' x", &values), "'' x");
    }

    #[test]
    fn an_empty_card_adds_nothing_before_the_prompt() {
        let none = CardText {
            id: CARD,
            title: "",
            description: "",
        };
        let got = render_task(
            &package(&[]),
            &stage("Do it."),
            &none,
            &params(&[]),
            &Handoff::default(),
        )
        .unwrap();
        assert!(got.starts_with("Do it.\n\nWhen you are done"), "{got}");
    }

    fn render(stage: &Stage, handoff: &Handoff) -> String {
        render_task(&package(&[]), stage, &card(), &params(&[]), handoff).unwrap()
    }

    fn inputs(inputs: Vec<InputView>) -> Handoff {
        Handoff {
            inputs,
            ..Handoff::default()
        }
    }

    #[test]
    fn a_small_text_input_is_written_into_the_task_in_a_fence_longer_than_its_own() {
        let got = render(
            &stage("Review."),
            &inputs(vec![InputView {
                name: "plan".into(),
                state: InputState::Text {
                    version: 2,
                    bytes: 14,
                    body: Some("use ``` here\n".into()),
                },
            }]),
        );
        assert!(
            got.contains("Input artifacts:\n- `plan` (text, version 2, 14 bytes):\n````\nuse ``` here\n\n````"),
            "{got}"
        );
    }

    #[test]
    fn a_large_text_input_is_named_and_a_file_input_says_how_to_download_it() {
        let got = render(
            &stage("Review."),
            &inputs(vec![
                InputView {
                    name: "spec".into(),
                    state: InputState::Text {
                        version: 1,
                        bytes: 20_000,
                        body: None,
                    },
                },
                InputView {
                    name: "logo".into(),
                    state: InputState::File {
                        version: 3,
                        size: 99,
                        media_type: "image/png".into(),
                        media_ref: "https://media.example.org/a.png".into(),
                    },
                },
                InputView {
                    name: "later".into(),
                    state: InputState::Missing,
                },
            ]),
        );
        assert!(got.contains("- `spec` (text, version 1, 20 KiB): too long to write here. Read it with the Beehive tool `read_artifact`."), "{got}");
        assert!(got.contains("- `logo` (file, version 3, image/png, 99 bytes): download it with `buzz media get https://media.example.org/a.png -o <path>`."), "{got}");
        assert!(got.contains("- `later`: not saved yet."), "{got}");
    }

    #[test]
    fn a_stage_with_outputs_says_how_to_save_a_document_and_a_file() {
        let mut s = stage("Write.");
        s.outputs = vec![
            crate::domain::workflow::Output {
                artifact: "plan".into(),
                ..Default::default()
            },
            crate::domain::workflow::Output {
                artifact: "logo".into(),
                optional: true,
                ..Default::default()
            },
        ];
        let channel = uuid::Uuid::from_u128(7);
        let got = render(
            &s,
            &Handoff {
                channel: Some(channel),
                ..Handoff::default()
            },
        );
        assert!(
            got.contains(
                "Outputs to save before you report a result:\n- `plan`\n- `logo` (optional)\n"
            ),
            "{got}"
        );
        assert!(
            got.contains(&format!("--channel {channel} --reply-to")),
            "{got}"
        );
        assert!(got.contains("kind `file`"), "{got}");
        // The outputs come before the report instruction.
        assert!(got.find("Outputs to save").unwrap() < got.find("When you are done").unwrap());
    }

    #[test]
    fn a_repeat_plan_is_named_as_an_output_and_the_brief_comes_before_the_inputs() {
        let mut s = stage("Work.");
        s.repeat = Some(crate::domain::workflow::Repeat {
            plan: "todo".into(),
            ..Default::default()
        });
        let got = render(
            &s,
            &Handoff {
                brief: "Earlier sessions worked on this stage.".into(),
                ..Handoff::default()
            },
        );
        assert!(got.contains("- `todo`: the plan."), "{got}");
        assert!(got.contains("`update_plan`"), "{got}");
        assert!(got.find("Earlier sessions").unwrap() < got.find("Outputs to save").unwrap());
    }

    #[test]
    fn a_stage_with_no_inputs_or_outputs_adds_neither_section() {
        let got = render(&stage("Do it."), &Handoff::default());
        assert!(
            !got.contains("Input artifacts") && !got.contains("Outputs to save"),
            "{got}"
        );
    }

    #[test]
    fn the_card_link_names_the_page_buzz_app_registers_and_the_card() {
        assert_eq!(
            card_link(CARD),
            "buzz://plugin?id=buzz.beehive&page=beehive&route.card=00000000-0000-0000-0000-00000000002a"
        );
        let got = with_card_link("Task text.", CARD);
        assert_eq!(
            got,
            "Task text.\n\nOpen the card in Beehive: buzz://plugin?id=buzz.beehive&page=beehive&route.card=00000000-0000-0000-0000-00000000002a"
        );
    }

    #[test]
    fn the_agent_is_told_the_repo_it_works_in_and_which_are_context() {
        let repos = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert_eq!(repos_section(&[], false), "");
        // The issue names its own repo already.
        assert_eq!(repos_section(&repos(&["example-org/api"]), true), "");
        assert_eq!(
            repos_section(&repos(&["example-org/api"]), false),
            "Work in the repo `example-org/api`."
        );
        let text = repos_section(&repos(&["example-org/api", "example-org/web"]), true);
        assert!(
            text.starts_with("Work in the repo `example-org/api`."),
            "{text}"
        );
        assert!(
            text.contains("`example-org/web`, which is context"),
            "{text}"
        );
    }
}
