//! The project layer: an immutable view of a project's `src/`, swapped whole.

pub mod describe;
pub mod diagnostic;
pub mod layout;
pub mod lexicons;
pub mod snapshot;
pub mod walk;

pub use snapshot::{
    ProjectLexicon, ProjectModule, ProjectScript, ProjectSnapshot, ScriptBody, SharedSnapshot,
    shared_empty,
};
