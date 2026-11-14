//! The message that asks a stage's agent to do the stage.
//!
//! Ported from the Go engine's `RenderPrompt` (`core/workflow/render.go`). The variables
//! that need an issue, a worktree or an artifact copy (`issue.url`, `branch`, `scratch`,
//! `artifacts.*`, `inputs.*`) render as empty text until the phases that supply them.

use std::collections::BTreeMap;

use crate::domain::workflow::Stage;
use crate::workflow::load::Package;
use crate::workflow::routes::reportable_results;
use crate::workflow::template::{find_vars, known_variable, param_variable, variables};

/// What the message says about the card.
pub struct CardText<'a> {
    pub title: &'a str,
    pub description: &'a str,
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
    parts.push(format!(
        "When you are done, call the Beehive tool `stage_done` with one of: {}.",
        results.join(", ")
    ));
    Ok(parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
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

    fn card() -> CardText<'static> {
        CardText {
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
        let got = render_task(&package(&[]), &s, &card(), &params(&[])).unwrap();
        // `needs_work` has no route here, so the agent is not offered it.
        let want = "the bug\n\nIt breaks.\n\n\
            Fix the bug.\nIt breaks.\nPick passed, failed, needs_input, blocked.\n\n\
            When you are done, call the Beehive tool `stage_done` with one of: passed, failed, needs_input, blocked.";
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
        let got = render_task(&package(&[]), &s, &card(), &params(&[])).unwrap();
        assert!(
            got.starts_with("/demo:review title the bug\n\nthe bug\n\nIt breaks.\n\n"),
            "rendered task:\n{got}"
        );
        assert!(got.ends_with("`stage_done` with one of: passed, failed, needs_input."));
    }

    #[test]
    fn a_command_with_no_args_is_the_bare_command() {
        let mut s = stage("");
        s.command = "review".into();
        let got = render_task(&package(&[]), &s, &card(), &params(&[])).unwrap();
        assert!(got.starts_with("/demo:review\n\nthe bug"), "{got}");
    }

    #[test]
    fn a_prompt_file_is_read_from_the_package() {
        let mut s = stage("");
        s.prompt_file = "prompts/p.md".into();
        let pkg = package(&[("prompts/p.md", "Look at {{issue.title}}\n")]);
        let got = render_task(&pkg, &s, &card(), &params(&[])).unwrap();
        assert!(
            got.contains("\n\nLook at the bug\n\nWhen you are done"),
            "{got}"
        );
        let mut missing = stage("");
        missing.prompt_file = "prompts/none.md".into();
        assert!(render_task(&pkg, &missing, &card(), &params(&[])).is_err());
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
            title: "",
            description: "",
        };
        let got = render_task(&package(&[]), &stage("Do it."), &none, &params(&[])).unwrap();
        assert!(got.starts_with("Do it.\n\nWhen you are done"), "{got}");
    }
}
