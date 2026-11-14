//! The JSON Schema of the workflow files, generated from the types in `domain::workflow`.
//!
//! The committed copy is `crates/buzz-hive-core/workflow.schema.json`. The app's generated
//! TypeScript types come from it, and a test fails when it drifts from the types.

use serde_json::{json, Value};

use crate::domain::workflow::{PackageFile, WorkflowFile, ROLES, STANDARD_RESULTS};
use crate::workflow::routes::{ROUTE_ASK, ROUTE_STOP};

/// The schema of `workflow.yaml`, with `kiln-package.yaml` as `$defs.PackageFile`.
pub fn generate() -> Value {
    let mut schema =
        serde_json::to_value(schemars::schema_for!(WorkflowFile)).expect("a schema serializes");
    let package =
        serde_json::to_value(schemars::schema_for!(PackageFile)).expect("a schema serializes");
    let root = schema.as_object_mut().expect("a schema is an object");
    root.insert(
        "title".into(),
        Value::String("Kiln workflow (workflow.yaml)".into()),
    );
    let defs = root
        .entry("$defs")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("$defs is an object");
    // The package schema's own definitions join the shared ones.
    if let Some(package_defs) = package.get("$defs").and_then(Value::as_object) {
        for (name, def) in package_defs {
            defs.entry(name.clone()).or_insert_with(|| def.clone());
        }
    }
    let mut package_root = package.clone();
    if let Some(object) = package_root.as_object_mut() {
        object.remove("$defs");
        object.remove("$schema");
    }
    defs.insert("PackageFile".into(), package_root);
    defs.insert(
        "StandardResult".into(),
        json!({
            "description": "The results every prompt stage may report.",
            "type": "string",
            "enum": STANDARD_RESULTS,
        }),
    );
    defs.insert(
        "RouteKeyword".into(),
        json!({
            "description": "Route targets beside stage ids.",
            "type": "string",
            "enum": [ROUTE_STOP, ROUTE_ASK],
        }),
    );
    defs.insert(
        "StageRole".into(),
        json!({
            "description": "What a stage can do to an issue or its pull request.",
            "type": "string",
            "enum": ROLES,
        }),
    );
    schema
}

/// The schema as the committed file holds it: pretty JSON with a trailing newline.
pub fn generate_text() -> String {
    let mut text = serde_json::to_string_pretty(&generate()).expect("a schema serializes");
    text.push('\n');
    text
}
