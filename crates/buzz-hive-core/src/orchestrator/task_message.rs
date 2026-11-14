//! The message that asks a stage's agent to do the stage.
//!
//! Ported from the Go engine's `RenderPrompt` (`core/workflow/render.go`). The variables
//! that need an issue, a worktree or a scratch folder (`issue.url`, `branch`, `scratch`,
//! `artifacts.*`, `inputs.*`) render as empty text. A stage reads its artifacts through the
//! input and output sections this message adds and the `read_artifact` and `save_artifact`
//! tools.

use std::collections::BTreeMap;

use crate::domain::workflow::Stage;
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

/// A text input under this many bytes is written into the task. A larger one is named, and
/// the agent reads it with `read_artifact`.
pub const INLINE_LIMIT: usize = 8 * 1024;

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
pub fn render_task(
    package: &Package,
    stage: &Stage,
    card: &CardText<'_>,
    params: &BTreeMap<String, String>,
    handoff: &Handoff,
) -> Result<String, String> {
    let text = package.prompt_text(stage)?;
    let results = reportable_results(stage);
    let mut values: BTreeMap<String, String> = variables()
        .into_iter()
        .filter(|v| !v.pattern)
        .map(|v| (v.name, String::new()))
        .collect();
    values.insert("issue.title".into(), card.title.to_owned());
    values.insert("issue.body".into(), card.description.to_owned());
    values.insert("stage.results".into(), results.join(", "));
    for (name, value) in params {
        values.insert(format!("params.{name}"), value.clone());
    }
    let card_block = match (card.title.is_empty(), card.description.is_empty()) {
        (_, true) => card.title.to_owned(),
        (true, false) => card.description.to_owned(),
        (false, false) => format!("{}\n\n{}", card.title, card.description),
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
    parts.push(handoff.brief.clone());
    parts.push(inputs_section(&handoff.inputs));
    parts.push(outputs_section(stage, card.id, handoff.channel));
    parts.push(format!(
        "When you are done, call the Beehive tool `stage_done` with card_id `{}` and result one of: {}.",
        card.id,
        results.join(", ")
    ));
    Ok(parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
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
                "- `{name}` (text, version {version}, {}): too long to write here. Read it with the Beehive tool `read_artifact`.",
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
            "- `{plan}`: the plan. Save it as a Markdown checklist, then check items off with the Beehive tool `update_plan`."
        ));
    }
    lines.push(format!(
        "Save a document with the Beehive tool `save_artifact`, with card_id `{card}`, the artifact's name and its full text as content. Each call saves a new version."
    ));
    let channel = channel.map_or_else(|| "<channel>".to_owned(), |c| c.to_string());
    lines.push(format!(
        "To save a file, such as an image or a PDF, upload it with `buzz upload file --file <path>`, post the file in your own thread with `buzz messages send --channel {channel} --reply-to <the id of the message that mentioned you> --file <path> --content <name>`, then call `save_artifact` with card_id `{card}`, the artifact's name, kind `file`, and the sha256, size and type the upload printed."
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
            &Handoff::default(),
        )
        .unwrap();
        // `needs_work` has no route here, so the agent is not offered it.
        let want = "the bug\n\nIt breaks.\n\n\
            Fix the bug.\nIt breaks.\nPick passed, failed, needs_input, blocked.\n\n\
            When you are done, call the Beehive tool `stage_done` with card_id `00000000-0000-0000-0000-00000000002a` and result one of: passed, failed, needs_input, blocked.";
        assert_eq!(got, want);
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
            "`stage_done` with card_id `00000000-0000-0000-0000-00000000002a` and result one of: passed, failed, needs_input."
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
}
