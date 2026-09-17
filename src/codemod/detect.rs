//! What a script still references, and what its names are bound to.
//!
//! Every rule in the rewrite turns on one question: is this name the host
//! global of the same name, or something the script itself bound? A global the
//! script has shadowed is not ours to touch, and that is also what makes the
//! rewrite idempotent — a migrated script binds `db`, `log`, `json` as locals,
//! so a second pass sees nothing of ours left.

use std::collections::HashSet;

use full_moon::ast::{Ast, Call, FunctionArgs, Index, Prefix, Suffix, Var};
use full_moon::node::Node;
use full_moon::tokenizer::{TokenReference, TokenType};
use full_moon::visitors::Visitor;

use super::ScriptKind;

/// The globals v3 removes, in the order `needs_migration` reports them.
pub const REMOVED_GLOBALS: [&str; 31] = [
    "db",
    "Record",
    "xrpc",
    "atproto",
    "linked_repos",
    "jobs",
    "job",
    "http",
    "log",
    "now",
    "TID",
    "json",
    "toarray",
    "params",
    "event",
    "record",
    "action",
    "uri",
    "did",
    "rkey",
    "src",
    "val",
    "neg",
    "cts",
    "exp",
    "method",
    "collection",
    "caller_did",
    "delegate_did",
    "env",
    "space",
];

/// One step after the head of a name reference: `db` `.get` `(uri)`.
pub enum Seg<'a> {
    Dot(&'a TokenReference),
    Bracket,
    Call(&'a FunctionArgs),
    Method(&'a TokenReference, &'a FunctionArgs),
}

/// A name reference and everything applied to it, flattened. Both a call
/// (`db.get(uri)`) and a plain read (`env.KEY`) reach the rewrite as one of
/// these, so a rule is written once rather than once per AST shape.
pub struct Path<'a> {
    pub head: &'a TokenReference,
    pub segs: Vec<Seg<'a>>,
}

impl<'a> Path<'a> {
    pub fn build(prefix: &'a Prefix, suffixes: impl Iterator<Item = &'a Suffix>) -> Option<Self> {
        let Prefix::Name(head) = prefix else {
            return None;
        };
        let mut segs = Vec::new();
        for suffix in suffixes {
            match suffix {
                Suffix::Index(Index::Dot { name, .. }) => segs.push(Seg::Dot(name)),
                Suffix::Index(_) => segs.push(Seg::Bracket),
                Suffix::Call(Call::AnonymousCall(args)) => segs.push(Seg::Call(args)),
                Suffix::Call(Call::MethodCall(method)) => {
                    segs.push(Seg::Method(method.name(), method.args()))
                }
                _ => return None,
            }
        }
        Some(Path { head, segs })
    }

    pub fn head_name(&self) -> Option<&str> {
        identifier(self.head)
    }

    /// The dotted names before the first call or bracket: `["spaces", "query"]`
    /// for `atproto.spaces.query{...}`.
    pub fn dots(&self) -> Vec<&'a str> {
        self.segs
            .iter()
            .map_while(|seg| match seg {
                Seg::Dot(name) => identifier(name),
                _ => None,
            })
            .collect()
    }

    /// The end byte of the head plus `n` dotted names.
    pub fn dot_end(&self, n: usize) -> Option<usize> {
        if n == 0 {
            return end_of(self.head);
        }
        match self.segs.get(n - 1)? {
            Seg::Dot(name) => end_of(*name),
            _ => None,
        }
    }

    pub fn start(&self) -> Option<usize> {
        start_of(self.head)
    }
}

pub fn identifier(token: &TokenReference) -> Option<&str> {
    match token.token_type() {
        TokenType::Identifier { identifier } => Some(identifier.as_str()),
        _ => None,
    }
}

pub fn start_of(node: &impl Node) -> Option<usize> {
    node.start_position().map(|p| p.bytes())
}

pub fn end_of(node: &impl Node) -> Option<usize> {
    node.end_position().map(|p| p.bytes())
}

/// One name the script binds, and the byte range over which that binding is
/// what the name means.
pub struct Binding {
    pub name: String,
    pub from: usize,
    pub to: usize,
}

/// What a script binds, where each binding reaches, and which bindings are its
/// own rather than the ones the v3 contract makes: `handle` itself and
/// `handle`'s parameters.
pub struct Bindings {
    /// Every name bound anywhere, used where the answer has to be
    /// conservative: whether a name is safe for the requires block to take.
    pub names: HashSet<String>,
    /// Names bound somewhere other than the contract's own two sites.
    pub shadowed: HashSet<String>,
    scopes: Vec<Binding>,
}

impl Bindings {
    /// Whether a use at `at` reads one of the script's own bindings rather
    /// than the host global of the same name. Ranges are widened rather than
    /// narrowed wherever a block's extent is uncertain, so the answer errs
    /// towards leaving the script's code alone.
    pub fn in_scope(&self, name: &str, at: usize) -> bool {
        self.scopes
            .iter()
            .any(|binding| binding.name == name && binding.from <= at && at < binding.to)
    }
}

pub fn bindings(ast: &Ast) -> Bindings {
    let mut scan = BindingScan {
        names: HashSet::new(),
        shadowed: HashSet::new(),
        scopes: Vec::new(),
        blocks: Vec::new(),
        body_is_handle: false,
    };
    scan.visit_ast(ast);
    Bindings {
        names: scan.names,
        shadowed: scan.shadowed,
        scopes: scan.scopes,
    }
}

struct BindingScan {
    names: HashSet<String>,
    shadowed: HashSet<String>,
    scopes: Vec<Binding>,
    /// Ends of the blocks currently open, so a `local` knows how far it
    /// reaches.
    blocks: Vec<usize>,
    /// Set by `handle`'s own declaration and consumed by the body that
    /// follows it, so its parameters are not read as the script shadowing
    /// `input` or `ctx`.
    body_is_handle: bool,
}

impl BindingScan {
    fn bind(&mut self, token: &TokenReference, from: usize, to: usize) {
        self.record(token, true, from, to);
    }

    fn record(&mut self, token: &TokenReference, shadows: bool, from: usize, to: usize) {
        let Some(name) = identifier(token) else {
            return;
        };
        self.names.insert(name.to_string());
        if shadows {
            self.shadowed.insert(name.to_string());
        }
        self.scopes.push(Binding {
            name: name.to_string(),
            from,
            to,
        });
    }

    /// Where the innermost open block ends. A `local` is visible from its own
    /// statement to there.
    fn block_end(&self) -> usize {
        self.blocks.last().copied().unwrap_or(usize::MAX)
    }
}

impl Visitor for BindingScan {
    fn visit_block(&mut self, node: &full_moon::ast::Block) {
        self.blocks.push(end_of(node).unwrap_or(usize::MAX));
    }

    fn visit_block_end(&mut self, _node: &full_moon::ast::Block) {
        self.blocks.pop();
    }

    fn visit_local_assignment(&mut self, node: &full_moon::ast::LocalAssignment) {
        // A local's own right-hand side still sees whatever the name meant
        // before it, so the binding starts where the statement ends.
        let from = end_of(node).unwrap_or(0);
        let to = self.block_end();
        for name in node.names() {
            self.bind(name, from, to);
        }
    }

    fn visit_local_function(&mut self, node: &full_moon::ast::LocalFunction) {
        let from = start_of(node).unwrap_or(0);
        let to = self.block_end();
        self.bind(node.name(), from, to);
    }

    fn visit_function_declaration(&mut self, node: &full_moon::ast::FunctionDeclaration) {
        let mut names = node.name().names().iter();
        let Some(first) = names.next() else { return };
        let is_handle = identifier(first) == Some("handle")
            && names.next().is_none()
            && node.name().method_name().is_none();
        // A `function f()` declaration assigns a global, which the whole file
        // sees.
        self.record(first, !is_handle, 0, usize::MAX);
        self.body_is_handle = is_handle;
    }

    fn visit_function_body(&mut self, node: &full_moon::ast::FunctionBody) {
        let is_handle = std::mem::take(&mut self.body_is_handle);
        let (from, to) = span(node);
        for parameter in node.parameters() {
            if let full_moon::ast::Parameter::Name(name) = parameter {
                self.record(name, !is_handle, from, to);
            }
        }
    }

    fn visit_generic_for(&mut self, node: &full_moon::ast::GenericFor) {
        // Lua evaluates the iterator expression in the enclosing scope, so a
        // name read there is whatever it meant before the loop.
        let from = end_of(node.do_token()).unwrap_or(0);
        let to = end_of(node).unwrap_or(usize::MAX);
        for name in node.names() {
            self.bind(name, from, to);
        }
    }

    fn visit_numeric_for(&mut self, node: &full_moon::ast::NumericFor) {
        let from = end_of(node.do_token()).unwrap_or(0);
        let to = end_of(node).unwrap_or(usize::MAX);
        self.bind(node.index_variable(), from, to);
    }

    fn visit_assignment(&mut self, node: &full_moon::ast::Assignment) {
        for var in node.variables() {
            if let Var::Name(name) = var {
                self.bind(name, 0, usize::MAX);
            }
        }
    }
}

fn span(node: &impl Node) -> (usize, usize) {
    (
        start_of(node).unwrap_or(0),
        end_of(node).unwrap_or(usize::MAX),
    )
}

/// The names a trigger puts in scope. A free `uri` is the record's URI in a
/// record script and an undefined global in a query, so reporting it depends
/// on the kind; `Unknown` reports every one rather than clear a script it
/// cannot judge.
const KIND_DEPENDENT: [&str; 15] = [
    "record",
    "action",
    "uri",
    "did",
    "collection",
    "rkey",
    "src",
    "val",
    "neg",
    "cts",
    "exp",
    "event",
    "params",
    "method",
    "job",
];

fn set_by(kind: ScriptKind) -> &'static [&'static str] {
    match kind {
        ScriptKind::Procedure | ScriptKind::Query => &["params", "method", "collection"],
        ScriptKind::RecordEvent => &[
            "event",
            "record",
            "action",
            "uri",
            "did",
            "collection",
            "rkey",
        ],
        ScriptKind::Label => &["event", "src", "uri", "val", "neg", "cts", "exp"],
        ScriptKind::Job => &["job"],
        ScriptKind::Unknown => &KIND_DEPENDENT,
    }
}

/// The removed globals a script still reaches as free names, empty for a
/// migrated script. A script `full_moon` cannot parse answers `["unparseable"]`
/// rather than nothing, so a broken script is never reported as one with
/// nothing left to do.
pub fn needs_migration(source: &str, kind: ScriptKind) -> Vec<&'static str> {
    let Ok(ast) = full_moon::parse(source) else {
        return vec!["unparseable"];
    };
    let mut scan = FreeScan {
        kind,
        bindings: bindings(&ast),
        seen: HashSet::new(),
    };
    scan.visit_ast(&ast);
    REMOVED_GLOBALS
        .iter()
        .copied()
        .filter(|name| scan.seen.contains(name))
        .collect()
}

struct FreeScan {
    kind: ScriptKind,
    bindings: Bindings,
    seen: HashSet<&'static str>,
}

impl FreeScan {
    fn record(&mut self, path: &Path<'_>) {
        let Some(name) = path.head_name() else {
            return;
        };
        if self.bindings.in_scope(name, path.start().unwrap_or(0)) {
            return;
        }
        let called = matches!(path.segs.first(), Some(Seg::Call(_)));
        // `log` is a function, and a table field or local named `log` that is
        // never called is not the global this rewrite is about.
        if name == "log" && !called {
            return;
        }
        if KIND_DEPENDENT.contains(&name) && !set_by(self.kind).contains(&name) {
            return;
        }
        if let Some(known) = REMOVED_GLOBALS.iter().find(|g| **g == name) {
            self.seen.insert(known);
        }
    }
}

impl Visitor for FreeScan {
    fn visit_var(&mut self, node: &Var) {
        match node {
            Var::Name(name) => {
                self.record(&Path {
                    head: name,
                    segs: Vec::new(),
                });
            }
            Var::Expression(expression) => {
                if let Some(path) = Path::build(expression.prefix(), expression.suffixes()) {
                    self.record(&path);
                }
            }
            _ => {}
        }
    }

    fn visit_function_call(&mut self, node: &full_moon::ast::FunctionCall) {
        if let Some(path) = Path::build(node.prefix(), node.suffixes()) {
            self.record(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_free_removed_global_in_a_fixed_order() {
        let source = r#"
            function handle()
              log(now())
              return db.get(params.uri)
            end
        "#;
        assert_eq!(
            needs_migration(source, ScriptKind::Query),
            vec!["db", "log", "now", "params"]
        );
    }

    #[test]
    fn a_migrated_script_needs_nothing() {
        let source = r#"
            local log = require("internal.logging")
            local db = require("happyview.db")
            function handle(input, ctx)
              log.info("x")
              return db.records(ctx.collection):run()
            end
        "#;
        assert!(needs_migration(source, ScriptKind::Query).is_empty());
    }

    #[test]
    fn a_shadowed_global_is_the_scripts_own_name() {
        let source = r#"
            function handle()
              local record = { title = "hi" }
              return record.title
            end
        "#;
        assert!(needs_migration(source, ScriptKind::RecordEvent).is_empty());
    }

    #[test]
    fn log_counts_only_where_it_is_called() {
        assert!(
            needs_migration(
                "function handle() return { log = log } end",
                ScriptKind::Query
            )
            .is_empty()
        );
        assert_eq!(
            needs_migration("function handle() log(\"x\") end", ScriptKind::Query),
            vec!["log"]
        );
    }

    #[test]
    fn a_global_read_in_a_for_header_is_reported() {
        let source = "function handle()\n  for _, record in ipairs(record.items) do end\nend\n";
        assert_eq!(
            needs_migration(source, ScriptKind::RecordEvent),
            vec!["record"]
        );
    }

    #[test]
    fn a_global_read_outside_its_shadow_is_still_reported() {
        let source = r#"
            function handle()
              local rows = db.get("at://x")
              for _, db in ipairs(rows) do end
              return rows
            end
        "#;
        assert_eq!(needs_migration(source, ScriptKind::Query), vec!["db"]);
    }

    #[test]
    fn a_name_the_trigger_does_not_set_is_not_reported() {
        let source = "function handle() return { rkey = rkey } end";
        assert_eq!(
            needs_migration(source, ScriptKind::RecordEvent),
            vec!["rkey"]
        );
        assert!(needs_migration(source, ScriptKind::Query).is_empty());
        assert_eq!(needs_migration(source, ScriptKind::Unknown), vec!["rkey"]);
    }

    #[test]
    fn an_unparseable_script_says_so_rather_than_nothing() {
        assert_eq!(
            needs_migration("function handle( end", ScriptKind::Query),
            vec!["unparseable"]
        );
    }

    #[test]
    fn handles_own_parameters_are_not_the_script_shadowing_them() {
        let ast = full_moon::parse("function handle(input, ctx) local x = 1 end").unwrap();
        let bindings = bindings(&ast);
        assert!(bindings.names.contains("input"));
        assert!(!bindings.shadowed.contains("input"));
        assert!(!bindings.shadowed.contains("ctx"));
        assert!(!bindings.shadowed.contains("handle"));
        assert!(bindings.shadowed.contains("x"));
    }

    #[test]
    fn a_local_of_the_same_name_is_the_script_shadowing_it() {
        let ast = full_moon::parse("function handle(input, ctx) local input = {} return input end")
            .unwrap();
        assert!(bindings(&ast).shadowed.contains("input"));
    }
}
