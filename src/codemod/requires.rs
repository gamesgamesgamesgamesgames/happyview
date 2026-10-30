//! The `require` block a rewritten script needs at its top.
//!
//! Names are fixed rather than derived, so two scripts migrated on different
//! days read the same and a reviewer can tell at a glance which module a call
//! belongs to.

use std::collections::BTreeSet;

use full_moon::ast::{Ast, Call, Expression, FunctionArgs, Prefix, Stmt, Suffix};
use full_moon::tokenizer::TokenType;

use super::detect::{identifier, start_of};
use super::edits::Edit;

pub struct Module {
    pub name: &'static str,
    pub path: &'static str,
}

/// Declaration order is the order the block is written in.
pub const MODULES: [Module; 14] = [
    Module {
        name: "log",
        path: "internal.logging",
    },
    Module {
        name: "time",
        path: "internal.time",
    },
    Module {
        name: "tids",
        path: "internal.tids",
    },
    Module {
        name: "json",
        path: "internal.json",
    },
    Module {
        name: "db",
        path: "happyview.db",
    },
    Module {
        name: "sql",
        path: "happyview.sql",
    },
    Module {
        name: "backlinks",
        path: "happyview.backlinks",
    },
    Module {
        name: "record",
        path: "happyview.record",
    },
    Module {
        name: "xrpc",
        path: "happyview.xrpc",
    },
    Module {
        name: "atproto",
        path: "happyview.atproto",
    },
    Module {
        name: "spaces",
        path: "happyview.spaces",
    },
    Module {
        name: "linked_repos",
        path: "happyview.linked_repos",
    },
    Module {
        name: "jobs",
        path: "happyview.jobs",
    },
    Module {
        name: "http",
        path: "happyview.http",
    },
];

pub fn module_path(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|m| m.name == name).map(|m| m.path)
}

/// Modules the script already binds under their canonical name. A require
/// under any other name is left alone and the canonical one is added beside
/// it, because every rewritten call site is written against the canonical name.
pub fn already_required(ast: &Ast) -> BTreeSet<&'static str> {
    let mut found = BTreeSet::new();
    for stmt in ast.nodes().stmts() {
        if let Some((name, path)) = require_binding(stmt)
            && let Some(module) = MODULES
                .iter()
                .find(|m| m.name == name && m.path == path)
                .map(|m| m.name)
        {
            found.insert(module);
        }
    }
    found
}

/// The insert for the modules `needed` that the script does not already bind,
/// followed by `blocks` — the shims and the hoisted declaration, which belong
/// under the requires they read and above the code that reads them.
pub fn block_edit(
    source: &str,
    ast: &Ast,
    needed: &BTreeSet<&'static str>,
    blocks: &[String],
) -> Option<Edit> {
    let existing = already_required(ast);
    let lines: Vec<String> = MODULES
        .iter()
        .filter(|m| needed.contains(m.name) && !existing.contains(m.name))
        .map(|m| format!("local {} = require(\"{}\")\n", m.name, m.path))
        .collect();
    if lines.is_empty() && blocks.is_empty() {
        return None;
    }
    let point = insertion_point(source, ast);
    let mut text = lines.concat();
    for block in blocks {
        // A blank line above each block, separating it from the requires
        // whether those are new or the script's own — but not at the very top
        // of a file, where there is nothing above to separate it from.
        if !text.is_empty() || matches!(point, Insertion::AfterRequires(_)) {
            text.push('\n');
        }
        text.push_str(block);
    }
    match point {
        // Joining the script's own require block keeps one run of requires
        // rather than two separated by a stray blank line.
        Insertion::AfterRequires(at) => {
            if !blocks.is_empty() && !line_is_blank(source, at) {
                text.push('\n');
            }
            Some(Edit::insert(at, text))
        }
        Insertion::BeforeCode(at) => Some(Edit::insert(at, format!("{text}\n"))),
    }
}

enum Insertion {
    AfterRequires(usize),
    BeforeCode(usize),
}

fn insertion_point(source: &str, ast: &Ast) -> Insertion {
    let mut last_require_end = None;
    for stmt in ast.nodes().stmts() {
        if require_binding(stmt).is_some() {
            last_require_end = super::detect::end_of(stmt);
        } else {
            break;
        }
    }
    if let Some(end) = last_require_end {
        return Insertion::AfterRequires(next_line_start(source, end));
    }
    let first = ast.nodes().stmts().next().and_then(start_of);
    // With nothing but comments in the file there is no code to sit above.
    Insertion::BeforeCode(first.map_or(source.len(), |at| line_start(source, at)))
}

fn line_start(source: &str, at: usize) -> usize {
    source[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// Whether the line beginning at `at` is already the blank one a block wants
/// under it.
fn line_is_blank(source: &str, at: usize) -> bool {
    source[at..]
        .split('\n')
        .next()
        .is_some_and(|line| line.trim().is_empty())
}

fn next_line_start(source: &str, at: usize) -> usize {
    source[at..].find('\n').map_or(source.len(), |i| at + i + 1)
}

fn require_binding(stmt: &Stmt) -> Option<(&str, &str)> {
    let Stmt::LocalAssignment(local) = stmt else {
        return None;
    };
    let mut names = local.names().iter();
    let name = identifier(names.next()?)?;
    if names.next().is_some() {
        return None;
    }
    let mut expressions = local.expressions().iter();
    let Expression::FunctionCall(call) = expressions.next()? else {
        return None;
    };
    if expressions.next().is_some() {
        return None;
    }
    let Prefix::Name(callee) = call.prefix() else {
        return None;
    };
    if identifier(callee)? != "require" {
        return None;
    }
    let mut suffixes = call.suffixes();
    let Suffix::Call(Call::AnonymousCall(args)) = suffixes.next()? else {
        return None;
    };
    if suffixes.next().is_some() {
        return None;
    }
    let literal = match args {
        FunctionArgs::String(token) => token,
        FunctionArgs::Parentheses { arguments, .. } => match arguments.iter().next()? {
            Expression::String(token) => token,
            _ => return None,
        },
        _ => return None,
    };
    match literal.token_type() {
        TokenType::StringLiteral { literal, .. } => Some((name, literal.as_str())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codemod::edits::apply;

    fn insert(source: &str, needed: &[&'static str]) -> String {
        blocks(source, needed, &[])
    }

    fn blocks(source: &str, needed: &[&'static str], blocks: &[String]) -> String {
        let ast = full_moon::parse(source).unwrap();
        let needed: BTreeSet<&'static str> = needed.iter().copied().collect();
        match block_edit(source, &ast, &needed, blocks) {
            Some(edit) => apply(source, &[edit]),
            None => source.to_string(),
        }
    }

    #[test]
    fn a_block_sits_under_the_requires_it_reads_and_above_the_code() {
        assert_eq!(
            blocks(
                "function handle() end\n",
                &["db"],
                &["local shim = 1\n".to_string()]
            ),
            "local db = require(\"happyview.db\")\n\nlocal shim = 1\n\nfunction handle() end\n"
        );
    }

    #[test]
    fn a_block_joins_a_script_that_already_has_its_requires() {
        assert_eq!(
            blocks(
                "local db = require(\"happyview.db\")\n\nfunction handle() end\n",
                &["db"],
                &["local shim = 1\n".to_string()]
            ),
            "local db = require(\"happyview.db\")\n\nlocal shim = 1\n\nfunction handle() end\n"
        );
    }

    #[test]
    fn a_fresh_block_sits_above_the_code_with_one_blank_line() {
        assert_eq!(
            insert("function handle() end\n", &["db", "log"]),
            "local log = require(\"internal.logging\")\nlocal db = require(\"happyview.db\")\n\nfunction handle() end\n"
        );
    }

    #[test]
    fn a_leading_comment_keeps_the_top_of_the_file() {
        assert_eq!(
            insert("-- why\n\nfunction handle() end\n", &["log"]),
            "-- why\n\nlocal log = require(\"internal.logging\")\n\nfunction handle() end\n"
        );
    }

    #[test]
    fn a_new_require_joins_the_scripts_own_block() {
        assert_eq!(
            insert(
                "local log = require(\"internal.logging\")\n\nfunction handle() end\n",
                &["log", "time"]
            ),
            "local log = require(\"internal.logging\")\nlocal time = require(\"internal.time\")\n\nfunction handle() end\n"
        );
    }

    #[test]
    fn a_module_already_bound_is_not_bound_twice() {
        let source = "local db = require(\"happyview.db\")\n\nfunction handle() end\n";
        assert_eq!(insert(source, &["db"]), source);
    }

    #[test]
    fn a_require_under_another_name_does_not_count_as_the_canonical_one() {
        let source = "local d = require(\"happyview.db\")\n\nfunction handle() end\n";
        assert_eq!(
            insert(source, &["db"]),
            "local d = require(\"happyview.db\")\nlocal db = require(\"happyview.db\")\n\nfunction handle() end\n"
        );
    }
}
