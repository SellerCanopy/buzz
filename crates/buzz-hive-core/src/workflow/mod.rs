//! Workflow packages: loading, checking and listing.
//!
//! Ported from the Go engine's `core/workflow`, `core/kinds` and `core/rules`. A package
//! is a folder holding `kiln-package.yaml`, `workflow.yaml` and optional prompt files.
//! [`check::check`] returns every problem in one, never only the first.
//!
//! Nothing here reads a disk. Files come through [`source::PackageSource`].

pub mod actions;
pub mod builtin;
pub mod check;
pub mod contents;
pub mod decode;
pub mod hash;
pub mod kinds;
pub mod load;
pub mod names;
pub mod options;
pub mod routes;
pub mod saved;
pub mod schema;
pub mod source;
pub mod template;
pub mod templates;
