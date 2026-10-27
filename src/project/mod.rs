//! The project layer: an immutable view of a project's `src/`, swapped whole.

pub mod boot;
pub mod build;
pub mod check;
pub mod declarations;
pub mod describe;
pub mod diagnostic;
pub mod layout;
pub mod lexicons;
pub mod modules;
pub mod prepare;
pub mod script_module;
pub mod snapshot;
pub mod walk;

pub use build::{BuildOptions, BuildReport, Diagnostic, Severity, build_snapshot, install};
pub use snapshot::{
    ProjectLexicon, ProjectModule, ProjectScript, ProjectSnapshot, ScriptBody, SharedSnapshot,
    shared_empty,
};
