//! Migrates a v2 Lua script onto the v3 contract: `handle(input, ctx)`,
//! `require("internal.*")` built-ins, `require("happyview.*")` libraries.
//!
//! One function does the work, so the CLI, the admin endpoint and the editor
//! cannot drift from each other. Where a construct has no mechanical
//! equivalent it is left in place under a `-- codemod:` line.

mod detect;
mod edits;
mod polyfills;
mod requires;
mod rewrite;

#[cfg(all(test, feature = "lua-reference"))]
mod behaviour;

pub use detect::{REMOVED_GLOBALS, UNPARSEABLE, needs_migration};
pub use rewrite::rewrite;

/// What the runner hands `handle` as its first argument. A procedure receives
/// its input body, a query its params, a record or label script its event, a
/// job its input — so the same free name means different things in different
/// scripts and cannot be rewritten without knowing which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Procedure,
    Query,
    RecordEvent,
    Label,
    Job,
    /// The kind is not known. Every name that depends on it is marked rather
    /// than guessed: a wrong guess reads as `nil` at run time, not as an error.
    Unknown,
}

impl ScriptKind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "procedure" => ScriptKind::Procedure,
            "query" => ScriptKind::Query,
            "record" => ScriptKind::RecordEvent,
            "label" => ScriptKind::Label,
            "job" => ScriptKind::Job,
            _ => return None,
        })
    }

    /// The kind a stored trigger id implies. Both spellings of the XRPC and
    /// labeler triggers are recognised, because the ids in the database are
    /// `xrpc.query:`, `xrpc.procedure:` and `labeler.apply:` while the
    /// contract names them by the kind they run as.
    pub fn from_trigger_id(id: &str) -> Self {
        for (prefix, kind) in [
            ("procedure.", ScriptKind::Procedure),
            ("xrpc.procedure:", ScriptKind::Procedure),
            ("query.", ScriptKind::Query),
            ("xrpc.query:", ScriptKind::Query),
            ("record.", ScriptKind::RecordEvent),
            ("label.", ScriptKind::Label),
            ("labeler.apply:", ScriptKind::Label),
            ("job.run:", ScriptKind::Job),
        ] {
            if id.starts_with(prefix) {
                return kind;
            }
        }
        ScriptKind::Unknown
    }

    /// How a marker names this kind to the person reading it.
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptKind::Procedure => "procedure",
            ScriptKind::Query => "query",
            ScriptKind::RecordEvent => "record",
            ScriptKind::Label => "label",
            ScriptKind::Job => "job",
            ScriptKind::Unknown => "unknown",
        }
    }
}

/// A construct a person still has to deal with, at the line it sits on in the
/// source that was handed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub source: String,
    pub notes: Vec<Note>,
}

#[derive(Debug, thiserror::Error)]
pub enum CodemodError {
    #[error("could not parse script: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_migrated_script_comes_back_byte_identical_with_no_notes() {
        let source = "local log = require(\"internal.logging\")\n\nfunction handle(input, ctx)\n  log.info(\"hi\")\nend\n";
        let result = rewrite(source, ScriptKind::Query).unwrap();
        assert_eq!(result.source, source);
        assert!(result.notes.is_empty());
    }

    #[test]
    fn rewriting_twice_changes_nothing_the_second_time() {
        let source = "function handle()\n  log(now())\n  return db.get(params.uri)\nend\n";
        let once = rewrite(source, ScriptKind::Query).unwrap().source;
        assert_eq!(rewrite(&once, ScriptKind::Query).unwrap().source, once);
    }

    #[test]
    fn a_marker_is_written_once_however_often_the_rewrite_runs() {
        let source = "function handle()\n  return TID.toNumber(params.tid)\nend\n";
        let once = rewrite(source, ScriptKind::Procedure).unwrap();
        assert_eq!(once.notes.len(), 1);
        let twice = rewrite(&once.source, ScriptKind::Procedure).unwrap();
        assert_eq!(twice.source, once.source);
        assert_eq!(twice.notes.len(), 1);
    }

    #[test]
    fn a_note_points_at_the_line_the_construct_is_on() {
        let source = "function handle()\n  local x = 1\n  return TID.toNumber(x)\nend\n";
        let notes = rewrite(source, ScriptKind::Procedure).unwrap().notes;
        assert_eq!(notes[0].line, 3);
    }

    #[test]
    fn an_unparseable_script_is_an_error_rather_than_a_guess() {
        let error = rewrite("function handle( end", ScriptKind::Query).unwrap_err();
        assert!(matches!(error, CodemodError::Parse(_)), "{error}");
    }

    #[test]
    fn comments_and_spacing_outside_a_rewrite_survive_it() {
        let source = "function handle()\n  --[[ keep\n       this ]]\n  local x   =   1 -- and this\n  log(x)\nend\n";
        let result = rewrite(source, ScriptKind::Query).unwrap().source;
        assert!(result.contains("--[[ keep\n       this ]]"), "{result}");
        assert!(result.contains("local x   =   1 -- and this"), "{result}");
    }

    #[test]
    fn a_trigger_id_names_the_kind_it_runs_as() {
        assert_eq!(
            ScriptKind::from_trigger_id("xrpc.query:com.example.list"),
            ScriptKind::Query
        );
        assert_eq!(
            ScriptKind::from_trigger_id("xrpc.procedure:com.example.create"),
            ScriptKind::Procedure
        );
        assert_eq!(
            ScriptKind::from_trigger_id("record.index:com.example.thing"),
            ScriptKind::RecordEvent
        );
        assert_eq!(
            ScriptKind::from_trigger_id("labeler.apply:com.example.thing"),
            ScriptKind::Label
        );
        assert_eq!(
            ScriptKind::from_trigger_id("job.run:app.example.export"),
            ScriptKind::Job
        );
        assert_eq!(
            ScriptKind::from_trigger_id("something.else"),
            ScriptKind::Unknown
        );
    }

    #[test]
    fn the_same_name_rewrites_differently_per_kind() {
        let source = "function handle()\n  return params.q\nend\n";
        let query = rewrite(source, ScriptKind::Query).unwrap();
        assert!(query.source.contains("input.q"), "{}", query.source);
        let procedure = rewrite(source, ScriptKind::Procedure).unwrap();
        assert!(
            procedure.source.contains("ctx.params.q"),
            "{}",
            procedure.source
        );
        let unknown = rewrite(source, ScriptKind::Unknown).unwrap();
        assert!(unknown.source.contains("params.q"), "{}", unknown.source);
        assert_eq!(unknown.notes.len(), 1);
    }
}
