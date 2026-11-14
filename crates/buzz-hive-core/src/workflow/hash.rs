//! The content hash that names a stored package version.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

/// The SHA-256 of a package's files: every path in sorted order, each with its content,
/// each part led by its length as 8 bytes big-endian, so no two packages collide by
/// shifting bytes between a name and a file. The same algorithm as the Go engine.
pub fn hash_files(files: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (name, content) in files {
        for part in [name.as_bytes(), content.as_bytes()] {
            hasher.update((part.len() as u64).to_be_bytes());
            hasher.update(part);
        }
    }
    hex::encode(hasher.finalize())
}
