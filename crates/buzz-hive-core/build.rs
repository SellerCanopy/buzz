//! Writes the table of built-in workflow files that `src/workflow/builtin.rs` includes.
//!
//! Each file under `workflows/` becomes one `include_bytes!` entry, so the binary carries
//! the packages and the stage templates without reading a disk at run time.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"))
        .join("workflows");
    println!("cargo:rerun-if-changed=workflows");
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    let mut table = String::from("&[\n");
    for (relative, absolute) in &files {
        writeln!(
            table,
            "    ({relative:?}, include_bytes!({:?})),",
            absolute.display().to_string()
        )
        .expect("a string takes writes");
    }
    table.push_str("]\n");
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets this")).join("builtin_files.rs");
    std::fs::write(out, table).expect("the build directory is writable");
}

/// Collects every file under `dir`, skipping the leftovers no package keeps.
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), ".git" | ".DS_Store" | ".svn" | ".hg") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            walk(root, &path, out);
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("the walk stays under the root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((relative, path));
        }
    }
}
