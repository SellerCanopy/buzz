//! A tool server an agent starts: one entry of its `mcp.json`.
//!
//! The same checks run where an admin sets a server, where the committed files are read once,
//! and in the host supervisor, which writes the file. A secret value never travels in a spec:
//! an `env` or `headers` value must name its secret as `${NAME}`, and the supervisor's host
//! fills it in (Phase 23).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

/// The most servers one agent may have.
pub const MAX_SERVERS_PER_AGENT: usize = 32;
/// The longest server name.
pub const MAX_NAME_CHARS: usize = 64;
const MAX_ARGS: usize = 64;
const MAX_ENTRIES: usize = 32;
const MAX_TEXT_CHARS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Http,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "stdio" => Some(Self::Stdio),
            "http" => Some(Self::Http),
            _ => None,
        }
    }
}

/// One server in an agent's `mcp.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolServer {
    pub name: String,
    pub transport: Transport,
    /// Stdio: `command`, `args`, `env`. Http: `url`, `headers`. Nothing else, and no `type`.
    pub spec: Value,
}

impl ToolServer {
    /// A server whose name and spec passed the checks. The spec is kept in its canonical
    /// form, so the same server always renders the same bytes.
    pub fn new(name: &str, transport: Transport, spec: &Value) -> Result<Self, String> {
        check_name(name)?;
        let canonical = match transport {
            Transport::Stdio => serde_json::to_value(stdio(spec)?),
            Transport::Http => serde_json::to_value(http(spec)?),
        }
        .map_err(|e| e.to_string())?;
        Ok(Self {
            name: name.to_owned(),
            transport,
            spec: canonical,
        })
    }

    /// The server an `mcp.json` entry describes. `type: "http"` or a `url` makes it an HTTP
    /// server, and anything else is a command to run.
    pub fn from_entry(name: &str, entry: &Value) -> Result<Self, String> {
        let object = entry
            .as_object()
            .ok_or_else(|| format!("server {name}: the entry must be an object"))?;
        let transport = match object.get("type").map(|t| t.as_str()) {
            None | Some(Some("stdio")) if !object.contains_key("url") => Transport::Stdio,
            None | Some(Some("http")) => Transport::Http,
            Some(other) => {
                return Err(format!(
                    "server {name}: type {other:?} is not supported; use stdio or http"
                ))
            }
        };
        let mut spec = object.clone();
        spec.remove("type");
        Self::new(name, transport, &Value::Object(spec))
    }

    /// The entry as `mcp.json` holds it.
    fn entry(&self) -> Entry<'_> {
        Entry {
            kind: (self.transport == Transport::Http).then_some("http"),
            spec: &self.spec,
        }
    }
}

#[derive(Serialize)]
struct Entry<'a> {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(flatten)]
    spec: &'a Value,
}

/// The text of an agent's `mcp.json` for these servers, by name, in one fixed layout.
pub fn render_mcp(servers: &[ToolServer]) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct File<'a> {
        mcp_servers: BTreeMap<&'a str, Entry<'a>>,
    }
    let file = File {
        mcp_servers: servers
            .iter()
            .map(|s| (s.name.as_str(), s.entry()))
            .collect(),
    };
    let mut text = serde_json::to_string_pretty(&file).expect("a server list serializes");
    text.push('\n');
    text
}

/// Checks a list a caller sent whole: its size, and that no name repeats.
pub fn check_list(servers: &[ToolServer]) -> Result<(), String> {
    if servers.len() > MAX_SERVERS_PER_AGENT {
        return Err(format!(
            "an agent may have at most {MAX_SERVERS_PER_AGENT} tool servers"
        ));
    }
    let mut names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(format!("the server {} is listed twice", pair[0]));
    }
    Ok(())
}

/// Whether `agent` can be the folder name of an agent: lowercase, digits and `-`, starting
/// with a letter, at most 24 characters.
pub fn check_agent(agent: &str) -> Result<(), String> {
    let mut chars = agent.chars();
    let valid = agent.len() <= 24
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "agent name {agent:?} must be 1 to 24 lowercase letters, digits or '-', starting with a letter"
        ))
    }
}

/// The servers an `mcp.json` file lists. The file is Claude Code's `.mcp.json`, so anything
/// beside `mcpServers` is ignored.
pub fn parse_mcp(text: &str) -> Result<Vec<ToolServer>, String> {
    let file: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let servers = file
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or("the file needs an mcpServers object")?;
    let servers = servers
        .iter()
        .map(|(name, entry)| ToolServer::from_entry(name, entry))
        .collect::<Result<Vec<_>, _>>()?;
    check_list(&servers)?;
    Ok(servers)
}

pub fn check_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid = name.chars().count() <= MAX_NAME_CHARS
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "server name {name:?} must be 1 to {MAX_NAME_CHARS} letters, digits, '_', '.' or '-', starting with a letter or digit"
        ))
    }
}

#[derive(Serialize)]
struct Stdio {
    command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct Http {
    url: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
}

fn only_keys(spec: &Value, allowed: &[&str]) -> Result<serde_json::Map<String, Value>, String> {
    let object = spec
        .as_object()
        .ok_or("the spec must be a JSON object")?
        .clone();
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "the spec has an unknown field {key:?}; it may hold {}",
            allowed.join(", ")
        ));
    }
    Ok(object)
}

fn text(value: &Value, what: &str) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{what} must be text"))?;
    if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS || text.contains(['\0', '\n']) {
        return Err(format!(
            "{what} must be 1 to {MAX_TEXT_CHARS} characters on one line"
        ));
    }
    Ok(text.to_owned())
}

fn stdio(spec: &Value) -> Result<Stdio, String> {
    let object = only_keys(spec, &["command", "args", "env"])?;
    let command = text(
        object
            .get("command")
            .ok_or("a stdio spec needs a command")?,
        "command",
    )?;
    let args = match object.get("args") {
        None => Vec::new(),
        Some(Value::Array(items)) if items.len() <= MAX_ARGS => items
            .iter()
            .map(|item| text(item, "an argument"))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(format!("args must be a list of at most {MAX_ARGS} texts")),
    };
    let env = references(object.get("env"), "env", |name| {
        name.chars().next().is_some_and(|c| !c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })?;
    Ok(Stdio { command, args, env })
}

fn http(spec: &Value) -> Result<Http, String> {
    let object = only_keys(spec, &["url", "headers"])?;
    let url = text(object.get("url").ok_or("an http spec needs a url")?, "url")?;
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(' ') {
        return Err("url must be an http:// or https:// address".into());
    }
    let headers = references(object.get("headers"), "headers", |name| {
        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })?;
    Ok(Http { url, headers })
}

/// An `env` or `headers` map. Every value must name a secret as `${NAME}`, so a value typed
/// into a form is refused, and the host fills in the real one.
fn references(
    map: Option<&Value>,
    what: &str,
    name_ok: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, String>, String> {
    let Some(map) = map else {
        return Ok(BTreeMap::new());
    };
    let object = map
        .as_object()
        .filter(|o| o.len() <= MAX_ENTRIES)
        .ok_or_else(|| format!("{what} must be an object of at most {MAX_ENTRIES} entries"))?;
    let mut out = BTreeMap::new();
    for (name, value) in object {
        if name.is_empty() || !name_ok(name) {
            return Err(format!("{what} has a name that is not allowed: {name:?}"));
        }
        let value = text(value, &format!("{what} {name}"))?;
        if !has_reference(&value) {
            return Err(format!(
                "{what} {name} must name its value as ${{NAME}}: a secret is never sent in a spec"
            ));
        }
        out.insert(name.clone(), value);
    }
    Ok(out)
}

/// Whether the text holds a `${NAME}` or `${NAME:-default}` reference.
fn has_reference(value: &str) -> bool {
    let mut rest = value;
    while let Some(at) = rest.find("${") {
        rest = &rest[at + 2..];
        let name = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .count();
        if name > 0
            && !rest.starts_with(|c: char| c.is_ascii_digit())
            && (rest[name..].starts_with('}') || rest[name..].starts_with(":-"))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The names the deploy's bash pattern (`AGENT_NAME_PATTERN` in `scripts/aws/lib.sh`) is
    /// tested on. Both sides read this one list, so they cannot drift apart unnoticed.
    const AGENT_NAMES: &str = include_str!("../../../../tests/aws/fixtures/agent-names.txt");

    #[test]
    fn check_agent_agrees_with_the_deploys_list_of_agent_names() {
        let mut seen = 0;
        for line in AGENT_NAMES.lines() {
            let Some((verdict, name)) = line.split_once(' ') else {
                continue;
            };
            if verdict.starts_with('#') {
                continue;
            }
            seen += 1;
            assert_eq!(
                check_agent(name).is_ok(),
                verdict == "ok",
                "{verdict} {name}"
            );
        }
        assert!(seen >= 10, "the list was read: {seen}");
    }

    #[test]
    fn an_entry_becomes_a_server_and_renders_back_the_same() {
        let stdio = ToolServer::from_entry(
            "filesystem",
            &json!({"command": "npx", "args": ["-y", "pkg"], "env": {"TOKEN": "${TOKEN}"}}),
        )
        .unwrap();
        let http = ToolServer::from_entry(
            "docs",
            &json!({"type": "http", "url": "https://mcp.example.org/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}}),
        )
        .unwrap();
        assert_eq!(stdio.transport, Transport::Stdio);
        assert_eq!(http.transport, Transport::Http);
        let text = render_mcp(&[stdio, http]);
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed,
            json!({"mcpServers": {
                "docs": {"type": "http", "url": "https://mcp.example.org/mcp", "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}},
                "filesystem": {"command": "npx", "args": ["-y", "pkg"], "env": {"TOKEN": "${TOKEN}"}},
            }})
        );
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn an_http_entry_without_a_type_is_found_by_its_url() {
        let server =
            ToolServer::from_entry("docs", &json!({"url": "https://mcp.example.org"})).unwrap();
        assert_eq!(server.transport, Transport::Http);
    }

    #[test]
    fn a_literal_secret_an_unknown_field_and_a_bad_name_are_refused() {
        for (name, transport, spec) in [
            (
                "a",
                Transport::Stdio,
                json!({"command": "x", "env": {"TOKEN": "sk-live-123"}}),
            ),
            (
                "a",
                Transport::Http,
                json!({"url": "https://x.example.org", "headers": {"Authorization": "Bearer abc"}}),
            ),
            ("a", Transport::Stdio, json!({"command": "x", "cwd": "/"})),
            ("a", Transport::Stdio, json!({"args": ["x"]})),
            ("a", Transport::Http, json!({"url": "ftp://x.example.org"})),
            ("a b", Transport::Stdio, json!({"command": "x"})),
            ("", Transport::Stdio, json!({"command": "x"})),
        ] {
            assert!(
                ToolServer::new(name, transport, &spec).is_err(),
                "{name} {spec}"
            );
        }
    }

    #[test]
    fn a_reference_needs_a_name() {
        assert!(has_reference("Bearer ${TOKEN}"));
        assert!(has_reference("${TOKEN:-none}"));
        assert!(!has_reference("${}"));
        assert!(!has_reference("${1X}"));
        assert!(!has_reference("$TOKEN"));
        assert!(!has_reference("${TOKEN"));
    }

    #[test]
    fn a_list_with_a_repeat_is_refused() {
        let one = ToolServer::new("a", Transport::Stdio, &json!({"command": "x"})).unwrap();
        assert!(check_list(std::slice::from_ref(&one)).is_ok());
        assert!(check_list(&[one.clone(), one]).is_err());
    }
}
