//! The committed JSON Schema is generated from the format types. Run with
//! `UPDATE_WORKFLOW_SCHEMA=1` to rewrite it after a type changes.

use buzz_hive_core::workflow::schema::generate_text;

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/workflow.schema.json");

#[test]
fn schema_file_matches_the_types() {
    let want = generate_text();
    if std::env::var_os("UPDATE_WORKFLOW_SCHEMA").is_some() {
        std::fs::write(PATH, &want).expect("write the schema");
        return;
    }
    let have = std::fs::read_to_string(PATH).expect("workflow.schema.json is committed");
    assert!(
        have == want,
        "workflow.schema.json is out of date: run `UPDATE_WORKFLOW_SCHEMA=1 cargo test -p buzz-hive-core --test workflow_schema`"
    );
}

#[test]
fn schema_names_every_stage_kind_and_role() {
    let text = generate_text();
    for kind in ["prompt", "action", "approval", "wait"] {
        assert!(text.contains(&format!("\"{kind}\"")), "kind {kind}");
    }
    for word in ["StageRole", "StandardResult", "RouteKeyword", "PackageFile"] {
        assert!(text.contains(word), "{word}");
    }
}

#[test]
fn schema_text_ends_with_one_newline() {
    let text = generate_text();
    assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
}
