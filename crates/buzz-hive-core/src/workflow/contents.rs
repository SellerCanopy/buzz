//! What a package holds beside its two Kiln files: the plugin content a stage can call.
//!
//! A smaller port of the Go engine's `contents.go`. It answers the two questions the
//! checks ask: does the package hold plugin content, and is a command callable.

use std::collections::BTreeMap;

use crate::domain::workflow::{PACKAGE_FILE, WORKFLOW_FILE};

/// The pieces of plugin content a package holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contents {
    /// Commands and callable skills, by name, sorted.
    pub commands: Vec<String>,
    pub skills: Vec<String>,
    pub subagents: Vec<String>,
    pub programs: Vec<String>,
    pub hooks: bool,
    pub servers: bool,
}

impl Contents {
    /// Whether the package holds anything a plugin would.
    pub fn has_plugin_content(&self) -> bool {
        !(self.commands.is_empty()
            && self.skills.is_empty()
            && self.subagents.is_empty()
            && self.programs.is_empty()
            && !self.hooks
            && !self.servers)
    }

    pub fn has_command(&self, name: &str) -> bool {
        self.commands.iter().any(|c| c == name)
    }
}

/// Classifies the files of a package.
pub fn contents_of(files: &BTreeMap<String, String>) -> Contents {
    let mut out = Contents::default();
    for (name, text) in files {
        if name == PACKAGE_FILE || name == WORKFLOW_FILE {
            continue;
        }
        let parts: Vec<&str> = name.split('/').collect();
        match parts.as_slice() {
            ["commands", file] if file.to_ascii_lowercase().ends_with(".md") => {
                out.commands.push(file[..file.len() - 3].to_owned());
            }
            ["skills", skill, file] if file.eq_ignore_ascii_case("SKILL.md") => {
                out.skills.push((*skill).to_owned());
                if callable(text) {
                    out.commands.push((*skill).to_owned());
                }
            }
            ["agents", .., file] if file.to_ascii_lowercase().ends_with(".md") => {
                out.subagents.push(name.clone());
            }
            ["bin", rest @ ..] if !rest.is_empty() && !name.ends_with('/') => {
                out.programs.push(rest.join("/"));
            }
            ["hooks", "hooks.json"] => out.hooks = true,
            [".mcp.json"] => out.servers = true,
            _ => {}
        }
    }
    out.commands.sort();
    out.commands.dedup();
    out
}

/// A skill is open to a slash call unless its frontmatter says `user-invocable: false`.
fn callable(skill_md: &str) -> bool {
    let Some((head, _)) = split_frontmatter(skill_md) else {
        return true;
    };
    match serde_yaml::from_str::<serde_yaml::Value>(&head) {
        Ok(serde_yaml::Value::Mapping(map)) => map
            .get("user-invocable")
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(true),
        _ => true,
    }
}

/// Splits a Markdown file into its YAML frontmatter, the text between an opening `---`
/// line at the very start and the next `---` line, and the body after that line.
pub fn split_frontmatter(text: &str) -> Option<(String, String)> {
    let text = text.replace("\r\n", "\n");
    let mut rest = text.strip_prefix("---\n")?;
    let mut head = String::new();
    loop {
        let (line, tail) = match rest.split_once('\n') {
            Some(pair) => pair,
            None => (rest, ""),
        };
        if line == "---" {
            return Some((head, tail.to_owned()));
        }
        if !rest.contains('\n') {
            return None;
        }
        head.push_str(line);
        head.push('\n');
        rest = tail;
    }
}
