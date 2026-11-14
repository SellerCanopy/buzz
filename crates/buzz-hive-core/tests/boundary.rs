//! The core must not depend on a database, web or Buzz crate.

#[test]
fn core_has_no_io_dependencies() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    // Only what ships counts. `[dev-dependencies]` may name this crate to turn on `testing`.
    let mut in_shipped_table = false;
    let mut shipped = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_shipped_table = line == "[dependencies]" || line == "[build-dependencies]";
        } else if in_shipped_table {
            shipped.push(line);
        }
    }
    assert!(!shipped.is_empty(), "found no [dependencies] to check");
    for banned in [
        "sqlx",
        "axum",
        "reqwest",
        "hyper",
        "tokio-tungstenite",
        "buzz-",
    ] {
        assert!(
            !shipped.iter().any(|l| l.starts_with(banned)),
            "buzz-hive-core must not depend on {banned}"
        );
    }
}
