//! Strict decoding of a package's YAML files.
//!
//! The Go engine walks a document against its Go types and reports every problem, not
//! only the first: an unknown field, a value of the wrong type. Each bad entry is dropped
//! before decoding, so the rest of the document still decodes. This does the same against
//! the JSON Schema that the types in `domain::workflow` generate, so the schema is the one
//! description of the format.
//!
//! Findings carry no line numbers: the YAML reader keeps no spans.

use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use serde_yaml::Value as Yaml;

use crate::domain::workflow::{
    Finding, PackageFile, Stage, WorkflowFile, PACKAGE_FILE, WORKFLOW_FILE,
};

/// Decodes a `kiln-package.yaml`.
pub fn decode_package(text: &str) -> (PackageFile, Vec<Finding>) {
    decode(PACKAGE_FILE, text, schema_of::<PackageFile>(), |_, _| {})
}

/// Decodes a `workflow.yaml`. A stage written the old way, with an `agent` mapping, a
/// `conversation` group or `execution.idle_timeout`, loses that key and gets one finding
/// that says where the setting lives now.
pub fn decode_workflow(text: &str) -> (WorkflowFile, Vec<Finding>) {
    decode(
        WORKFLOW_FILE,
        text,
        schema_of::<WorkflowFile>(),
        |doc, findings| {
            let Some(stages) = doc.get_mut("stages").and_then(Yaml::as_sequence_mut) else {
                return;
            };
            for (i, stage) in stages.iter_mut().enumerate() {
                strip_removed_keys(stage, &format!("stages[{i}]"), WORKFLOW_FILE, findings);
            }
        },
    )
}

/// Decodes one stage, as a stage template file holds it. Returns the stage and the
/// cleaned document as JSON, in the form `workflow.yaml`'s stages take.
pub fn decode_stage(file: &str, text: &str) -> (Stage, Option<Json>, Vec<Finding>) {
    let mut json = None;
    let (stage, findings) = decode_inner(
        file,
        text,
        schema_of::<Stage>(),
        |doc, findings| strip_removed_keys(doc, "", file, findings),
        &mut json,
    );
    (stage, json, findings)
}

fn decode<T: DeserializeOwned + Default>(
    file: &str,
    text: &str,
    schema: &'static Json,
    prepare: impl FnOnce(&mut Yaml, &mut Vec<Finding>),
) -> (T, Vec<Finding>) {
    decode_inner(file, text, schema, prepare, &mut None)
}

fn decode_inner<T: DeserializeOwned + Default>(
    file: &str,
    text: &str,
    schema: &'static Json,
    prepare: impl FnOnce(&mut Yaml, &mut Vec<Finding>),
    cleaned: &mut Option<Json>,
) -> (T, Vec<Finding>) {
    let mut doc: Yaml = match serde_yaml::from_str(text) {
        Ok(doc) => doc,
        Err(error) => {
            return (
                T::default(),
                vec![Finding::error(file, "yaml_invalid", error.to_string())],
            );
        }
    };
    let mut findings = Vec::new();
    if doc.is_null() {
        return (T::default(), findings);
    }
    prepare(&mut doc, &mut findings);
    let mut walker = Walker {
        file,
        root: schema,
        findings,
    };
    let walked = walker.walk(&doc, schema, "");
    let mut findings = walker.findings;
    let Some(json) = walked else {
        return (T::default(), findings);
    };
    match serde_json::from_value::<T>(json.clone()) {
        Ok(value) => {
            *cleaned = Some(json);
            (value, findings)
        }
        Err(error) => {
            findings.push(Finding::error(file, "yaml_invalid", error.to_string()));
            (T::default(), findings)
        }
    }
}

/// Removes the settings that moved to Buzz, with one finding each.
fn strip_removed_keys(stage: &mut Yaml, at: &str, file: &str, findings: &mut Vec<Finding>) {
    let Some(map) = stage.as_mapping_mut() else {
        return;
    };
    let path = |field: &str| {
        if at.is_empty() {
            format!("{file}:{field}")
        } else {
            format!("{file}:{at}.{field}")
        }
    };
    if map.get("agent").is_some_and(Yaml::is_mapping) {
        map.remove("agent");
        findings.push(Finding::error(
            path("agent"),
            "agent_object_removed",
            "set profile, model and effort on the agent in Buzz; agent here is the agent's name",
        ));
    }
    if map.get("conversation").is_some_and(|v| !v.is_null()) {
        map.remove("conversation");
        findings.push(Finding::error(
            path("conversation"),
            "conversation_removed",
            "replies reach the agent through its Buzz thread, so a stage has no conversation group",
        ));
    }
    if let Some(execution) = map.get_mut("execution").and_then(Yaml::as_mapping_mut) {
        if execution.get("idle_timeout").is_some_and(|v| !v.is_null()) {
            execution.remove("idle_timeout");
            findings.push(Finding::error(
                path("execution.idle_timeout"),
                "idle_timeout_removed",
                "the Buzz harness has its own idle timeout",
            ));
        }
    }
}

/// The schema of `T` as JSON, built once.
pub fn schema_of<T: JsonSchema + 'static>() -> &'static Json {
    // One cell per type, found by type id.
    use std::any::TypeId;
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CELLS: OnceLock<Mutex<HashMap<TypeId, &'static Json>>> = OnceLock::new();
    let cells = CELLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cells = cells.lock().expect("the schema cache is not poisoned");
    cells.entry(TypeId::of::<T>()).or_insert_with(|| {
        let schema = schemars::schema_for!(T);
        Box::leak(Box::new(
            serde_json::to_value(schema).expect("a schema serializes"),
        ))
    })
}

struct Walker<'a> {
    file: &'a str,
    root: &'a Json,
    findings: Vec<Finding>,
}

impl Walker<'_> {
    fn add(&mut self, path: &str, code: &str, message: String) {
        let at = if path.is_empty() {
            self.file.to_owned()
        } else {
            format!("{}:{path}", self.file)
        };
        self.findings.push(Finding::error(at, code, message));
    }

    /// Follows `$ref`s to the schema they name.
    fn resolve<'s>(&'s self, mut schema: &'s Json) -> &'s Json {
        while let Some(reference) = schema.get("$ref").and_then(Json::as_str) {
            let pointer = reference.strip_prefix('#').unwrap_or(reference);
            match self.root.pointer(pointer) {
                Some(target) => schema = target,
                None => break,
            }
        }
        schema
    }

    /// Checks `value` against `schema` and returns it as cleaned JSON. `None` means the
    /// value is null, or was invalid and is dropped (after a finding).
    fn walk(&mut self, value: &Yaml, schema: &Json, path: &str) -> Option<Json> {
        let value = untag(value);
        if value.is_null() {
            return None;
        }
        let schema = self.resolve(schema).clone();
        let mut atomic = false;
        let schema = match self.pick_alternative(value, &schema, path, &mut atomic) {
            Pick::Schema(picked) => picked,
            Pick::Reported => return None,
        };
        let kind = schema_type(&schema);
        match kind.as_deref() {
            Some("object") => self.object(value, &schema, path),
            Some("array") => self.array(value, &schema, path, atomic),
            Some("string") => self.string(value, &schema, path),
            Some("integer") => self.integer(value, path),
            Some("boolean") => match value {
                Yaml::Bool(b) => Some(Json::Bool(*b)),
                Yaml::Mapping(_) | Yaml::Sequence(_) => {
                    self.add(path, "invalid_value", "want a single value".into());
                    None
                }
                other => {
                    self.add(
                        path,
                        "invalid_value",
                        format!("{} is not a valid true or false", scalar_text(other)),
                    );
                    None
                }
            },
            _ => Some(yaml_to_json(value)),
        }
    }

    /// For a schema that is an `anyOf` or `oneOf`, the alternative the value's shape
    /// selects. A schema that is not one comes back as it is.
    fn pick_alternative(
        &mut self,
        value: &Yaml,
        schema: &Json,
        path: &str,
        atomic: &mut bool,
    ) -> Pick {
        let Some(alternatives) = schema
            .get("anyOf")
            .or_else(|| schema.get("oneOf"))
            .and_then(Json::as_array)
        else {
            return Pick::Schema(schema.clone());
        };
        let alternatives: Vec<Json> = alternatives
            .iter()
            .map(|a| self.resolve(a).clone())
            .filter(|a| schema_type(a).as_deref() != Some("null"))
            .collect();
        if alternatives.len() == 1 {
            return Pick::Schema(alternatives[0].clone());
        }
        let wanted: &[&str] = match value {
            Yaml::Mapping(_) => &["object"],
            Yaml::Sequence(_) => &["array"],
            Yaml::String(_) => &["string"],
            Yaml::Bool(_) => &["boolean", "string"],
            Yaml::Number(_) => &["integer", "number", "string"],
            _ => &[],
        };
        for want in wanted {
            if let Some(found) = alternatives
                .iter()
                .find(|a| schema_type(a).as_deref() == Some(want))
            {
                // A list that is one alternative of several is read whole: one bad entry
                // drops the argument, as the Go decoder does for an action argument.
                *atomic = *want == "array";
                return Pick::Schema(found.clone());
            }
        }
        self.add(
            path,
            "invalid_value",
            "want a value or a list of values".into(),
        );
        Pick::Reported
    }

    fn object(&mut self, value: &Yaml, schema: &Json, path: &str) -> Option<Json> {
        let Some(map) = value.as_mapping() else {
            self.add(
                path,
                "invalid_value",
                "want a mapping of names to values".into(),
            );
            return None;
        };
        let properties = schema.get("properties").and_then(Json::as_object);
        let extra = schema.get("additionalProperties").filter(|a| a.is_object());
        let title = if path.is_empty() {
            self.file.to_owned()
        } else {
            describe(schema)
        };
        let mut out = serde_json::Map::new();
        for (key, entry) in map {
            let Some(key) = key_text(key) else {
                self.add(path, "invalid_value", "a key must be a single value".into());
                continue;
            };
            let key_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            let entry_schema = match (properties.and_then(|p| p.get(&key)), extra) {
                (Some(property), _) => property.clone(),
                (None, Some(extra)) if properties.is_none() => extra.clone(),
                _ => {
                    self.add(
                        &key_path,
                        "unknown_field",
                        format!("{key} is not a field of {title}"),
                    );
                    continue;
                }
            };
            if let Some(cleaned) = self.walk(entry, &entry_schema, &key_path) {
                out.insert(key, cleaned);
            }
        }
        Some(Json::Object(out))
    }

    fn array(&mut self, value: &Yaml, schema: &Json, path: &str, atomic: bool) -> Option<Json> {
        let Some(items) = value.as_sequence() else {
            self.add(path, "invalid_value", "want a list".into());
            return None;
        };
        let item_schema = schema.get("items").cloned().unwrap_or(Json::Bool(true));
        let mut out = Vec::new();
        let mut failed = false;
        for (i, item) in items.iter().enumerate() {
            let item_path = format!("{path}[{i}]");
            match self.walk(item, &item_schema, &item_path) {
                Some(cleaned) => out.push(cleaned),
                None if !untag(item).is_null() => failed = true,
                None => {}
            }
        }
        if atomic && failed {
            return None;
        }
        Some(Json::Array(out))
    }

    fn string(&mut self, value: &Yaml, schema: &Json, path: &str) -> Option<Json> {
        if matches!(value, Yaml::Mapping(_) | Yaml::Sequence(_)) {
            self.add(path, "invalid_value", "want a single value".into());
            return None;
        }
        let text = scalar_text(value);
        if schema.get("format").and_then(Json::as_str) == Some("go-duration") {
            if let Err(error) = crate::domain::workflow::Duration::parse(&text) {
                self.add(
                    path,
                    "invalid_value",
                    format!("{text:?} is not a valid duration such as 30m: {error}"),
                );
                return None;
            }
        }
        if let Some(allowed) = schema.get("enum").and_then(Json::as_array) {
            let names: Vec<&str> = allowed.iter().filter_map(Json::as_str).collect();
            if !names.contains(&text.as_str()) {
                self.add(
                    path,
                    "invalid_value",
                    format!("want one of {}, got {text:?}", names.join(", ")),
                );
                return None;
            }
        }
        Some(Json::String(text))
    }

    fn integer(&mut self, value: &Yaml, path: &str) -> Option<Json> {
        match value {
            Yaml::Number(n) if n.is_i64() => n.as_i64().map(Json::from),
            Yaml::Mapping(_) | Yaml::Sequence(_) => {
                self.add(path, "invalid_value", "want a single value".into());
                None
            }
            other => {
                self.add(
                    path,
                    "invalid_value",
                    format!("{:?} is not a valid whole number", scalar_text(other)),
                );
                None
            }
        }
    }
}

enum Pick {
    Schema(Json),
    /// The value fits no alternative and the finding is already added.
    Reported,
}

/// The one non-null `type` of a schema.
fn schema_type(schema: &Json) -> Option<String> {
    match schema.get("type") {
        Some(Json::String(t)) => Some(t.clone()),
        Some(Json::Array(types)) => types
            .iter()
            .filter_map(Json::as_str)
            .find(|t| *t != "null")
            .map(str::to_owned),
        _ => {
            if schema.get("properties").is_some() {
                Some("object".into())
            } else if schema.get("enum").is_some() {
                Some("string".into())
            } else {
                None
            }
        }
    }
}

/// What a message calls the thing a field is not part of: the schema's title in words.
fn describe(schema: &Json) -> String {
    match schema.get("title").and_then(Json::as_str) {
        Some("Stage") => "a stage".to_owned(),
        Some("WorkflowFile") => WORKFLOW_FILE.to_owned(),
        Some("PackageFile") => PACKAGE_FILE.to_owned(),
        Some(title) => format!("the {} group", title.to_lowercase()),
        None => "this group".to_owned(),
    }
}

fn untag(value: &Yaml) -> &Yaml {
    match value {
        Yaml::Tagged(tagged) => untag(&tagged.value),
        other => other,
    }
}

fn key_text(key: &Yaml) -> Option<String> {
    match untag(key) {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn scalar_text(value: &Yaml) -> String {
    match value {
        Yaml::String(s) => s.clone(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Bool(b) => b.to_string(),
        Yaml::Null => String::new(),
        _ => String::new(),
    }
}

fn yaml_to_json(value: &Yaml) -> Json {
    match untag(value) {
        Yaml::Null => Json::Null,
        Yaml::Bool(b) => Json::Bool(*b),
        Yaml::Number(n) => serde_json::to_value(n).unwrap_or(Json::Null),
        Yaml::String(s) => Json::String(s.clone()),
        Yaml::Sequence(items) => Json::Array(items.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(map) => Json::Object(
            map.iter()
                .filter_map(|(k, v)| key_text(k).map(|k| (k, yaml_to_json(v))))
                .collect(),
        ),
        Yaml::Tagged(_) => Json::Null,
    }
}
