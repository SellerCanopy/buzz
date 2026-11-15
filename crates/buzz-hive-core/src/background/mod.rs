//! What the engine does on its own clock: reading the code host for new issues and comments.
//!
//! The passes here read the host and commit what they find, one transaction each. The loops
//! that call them on a timer are in the `buzz-hive` binary, beside the other loops.

pub mod comments;
pub mod intake;
