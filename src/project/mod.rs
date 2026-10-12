//! The project layer: an immutable view of a project's `src/`, swapped whole.

pub mod snapshot;

pub use snapshot::{
    ProjectLexicon, ProjectModule, ProjectScript, ProjectSnapshot, ScriptBody, SharedSnapshot,
    shared_empty,
};
