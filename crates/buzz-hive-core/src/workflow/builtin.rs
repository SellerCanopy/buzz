//! The workflow packages and stage templates every build ships, embedded in the binary.
//!
//! The files live under `crates/buzz-hive-core/workflows/`: `packages/<name>/...` for the
//! packages and `templates/<id>.yaml` for the stage templates. `build.rs` lists them.

use std::collections::BTreeMap;

use crate::workflow::source::{MemSource, Source};

/// The version a built-in package reports: the engine's own, so a built-in package follows
/// the release that ships it.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every embedded file, by path under `workflows/`.
static FILES: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/builtin_files.rs"));

/// The built-in packages as a source. Its label is `builtin`.
pub fn builtin_source() -> Source {
    let mut files = MemSource::new();
    for (path, bytes) in FILES {
        if let Some(inside) = path.strip_prefix("packages/") {
            files.insert(inside, bytes);
        }
    }
    Source::new("builtin", true, files)
}

/// The stage template files, by file name, such as `implement.yaml`.
pub fn builtin_templates() -> BTreeMap<String, String> {
    FILES
        .iter()
        .filter_map(|(path, bytes)| {
            let name = path.strip_prefix("templates/")?;
            Some((name.to_owned(), String::from_utf8_lossy(bytes).into_owned()))
        })
        .collect()
}
