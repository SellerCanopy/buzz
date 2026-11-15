//! A card's artifacts: documents a stage writes for the next one, and pointers to files in
//! the Buzz media store. Ported from the Go engine's `core/domain/artifacts.go`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// The largest text version the engine saves.
pub const MAX_ARTIFACT_BYTES: usize = 1 << 20;

/// Whether `name` is a safe artifact name: `^[a-z0-9][a-z0-9_-]{0,63}$`.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// `Err` with the reason when `name` is not a safe artifact name.
pub fn check_name(name: &str) -> Result<(), String> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(format!(
            "artifact name {name:?} must match ^[a-z0-9][a-z0-9_-]{{0,63}}$"
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Text,
    File,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::File => "file",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

/// What one version holds: a document, or a pointer to a blob in the media store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactContent {
    Text(String),
    File {
        /// Lowercase hex, 64 characters.
        sha256: String,
        size: i64,
        media_type: String,
        /// The blob's URL when the stage gave one, else the hash.
        media_ref: String,
    },
}

impl ArtifactContent {
    pub fn kind(&self) -> ArtifactKind {
        match self {
            Self::Text(_) => ArtifactKind::Text,
            Self::File { .. } => ArtifactKind::File,
        }
    }

    /// The content's hash: the file's own, or the SHA-256 of a document's bytes.
    pub fn sha256_hex(&self) -> String {
        match self {
            Self::Text(body) => {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(body.as_bytes()))
            }
            Self::File { sha256, .. } => sha256.clone(),
        }
    }
}

/// One artifact of a card, with its latest version number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactInfo {
    pub name: String,
    pub kind: ArtifactKind,
    pub latest: i32,
    /// Whether later stages that declare no inputs get the artifact.
    pub handoff: bool,
}

/// One saved version of an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactVersion {
    pub card_id: Uuid,
    pub name: String,
    pub version: i32,
    pub content: ArtifactContent,
    pub stage: String,
    /// The visit of `stage` that saved it.
    pub visit: u32,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// `Err` with the reason when `sha256` is not 64 lowercase hex characters.
pub fn check_sha256(sha256: &str) -> Result<(), String> {
    if sha256.len() == 64
        && sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err("sha256 must be 64 lowercase hex characters".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_pattern() {
        for ok in ["plan", "a", "0", "design-doc_2", &"a".repeat(64)] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-plan",
            "_plan",
            "Plan",
            "a b",
            "a/b",
            "a.md",
            &"a".repeat(65),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_hash_is_64_lowercase_hex() {
        assert!(check_sha256(&"a".repeat(64)).is_ok());
        assert!(check_sha256(&"A".repeat(64)).is_err());
        assert!(check_sha256(&"a".repeat(63)).is_err());
        assert!(check_sha256("").is_err());
    }
}
