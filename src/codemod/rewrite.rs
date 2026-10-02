//! The rewrite table, applied.
//!
//! A construct is either rewritten mechanically or left exactly as it was and
//! preceded by a `-- codemod:` line. There is deliberately no third option: a
//! script that half-migrated silently would read as finished while behaving
//! like neither version.

use std::collections::{BTreeSet, HashSet};

use full_moon::ast::{
    Ast, BinOp, Block, Expression, Field, FunctionArgs, FunctionCall, LastStmt, Stmt,
    TableConstructor, UnOp, Var,
};
use full_moon::visitors::Visitor;

use super::detect::{self, Bindings, Path, Seg, bindings, end_of, identifier, start_of};
use super::edits::{Edit, Piece, apply};
use super::polyfills::{self, Polyfill};
use super::requires;
use super::{CodemodError, Note, Rewrite, ScriptKind};

const MARK_DB_QUERY: &str = "db.query has options this rewrite cannot map -- rebuild it as a db.records(...) chain from require(\"happyview.db\")";
const MARK_DB_SEARCH: &str = "db.search has options this rewrite cannot map -- rewrite it as db.search(collection, field, query, limit) from require(\"happyview.db\")";
const MARK_DB_BACKLINKS: &str = "db.backlinks has options this rewrite cannot map -- rebuild it as a backlinks.to(...) chain from require(\"happyview.backlinks\")";
const MARK_LINKED_FIELD: &str =
    "a linked-repos handle carries no fields -- read this one from linked_repos.list()";
const MARK_SPACE_FIELD: &str =
    "v3 space handles carry no fields -- read them from spaces.info(uri)";
const MARK_SPACE_RETURN: &str = "v3 space handles carry no fields, so returning one answers nothing -- return spaces.info(uri) or the fields you need";
const MARK_AUTHOR_DID: &str =
    "v3 space records spell it author_did -- rename this read to author_did";
const MARK_HANDLE_PARAMS: &str = "handle takes exactly (input, ctx) in v3 -- cut this parameter list down to two, then re-run the codemod";
const MARK_READ_AT_LOAD: &str = "this runs while the script loads, before handle receives input and ctx -- move the read into handle, or into a function handle calls";
const MARK_NO_HANDLE: &str = "input and ctx are handle's parameters in v3 -- declare function handle(input, ctx) and read them there";

fn never_nil_message(name: &str) -> String {
    format!(
        "'{name}' is a handle and v3 handles are never nil -- test existence with spaces.info(uri) or linked_repos.list()"
    )
}

/// A linked-repo grant's fields, which its v3 handle does not carry.
const GRANT_FIELDS: [&str; 4] = ["did", "handle", "status", "scopes"];

/// What a blob download's fields are called on the other side.
const BLOB_FIELDS: [(&str, &str); 2] = [("handle", "bytes"), ("mimeType", "mime_type")];

/// The names this rewrite puts in scope itself.
const INTRODUCED: [&str; 3] = ["input", "ctx", "handle"];

/// What the v3 contract calls `handle`'s two parameters.
const CONTRACT_PARAMS: [&str; 2] = ["input", "ctx"];

/// What `handle`'s parameters are called once `input` and `ctx` have been
/// hoisted to file scope, where code outside `handle` can read them.
const HOISTED_PARAMS: [&str; 2] = ["handle_input", "handle_ctx"];

/// What a replacement's leading identifier needs to still mean what the
/// contract says it means.
fn introduced_name(text: &str) -> Option<&'static str> {
    let head: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    INTRODUCED.iter().copied().find(|name| *name == head)
}

/// One message for every name the script has taken, whether it is a module
/// this rewrite needs to bind or one it needs to read: the reader's next move
/// is a rename either way, and two wordings for that read as two problems.
fn blocked_message(name: &str) -> String {
    format!(
        "'{name}' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand"
    )
}

pub fn rewrite(source: &str, kind: ScriptKind) -> Result<Rewrite, CodemodError> {
    let ast = full_moon::parse(source).map_err(|errors| {
        CodemodError::Parse(
            errors
                .iter()
                .map(|e| e.error_message().into_owned())
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;

    let mut planner = Planner::new(source, &ast, kind);
    planner.visit_ast(&ast);
    let declare_hoist = planner.plan_handle(&ast);
    // The requires block goes in before the markers so that where both land on
    // the first line, the block sits above the comment it does not belong to.
    let mut blocks: Vec<String> = polyfills::ALL
        .iter()
        .filter(|polyfill| planner.polyfills.contains(polyfill.name))
        .map(|polyfill| polyfill.block(&planner.shimmed))
        .collect();
    if declare_hoist {
        blocks.push(format!(
            "local {}, {}\n",
            CONTRACT_PARAMS[0], CONTRACT_PARAMS[1]
        ));
    }
    if let Some(edit) = requires::block_edit(source, &ast, &planner.needed, &blocks) {
        planner.edits.push(edit);
    }
    planner.place_markers();

    let mut notes = std::mem::take(&mut planner.notes);
    notes.sort_by_key(|note| note.line);

    Ok(Rewrite {
        source: apply(source, &planner.edits),
        notes,
    })
}

/// The globals whose meaning comes from the trigger rather than the runtime.
fn kind_global(kind: ScriptKind, name: &str) -> Option<&'static str> {
    let table: &[(&str, &str)] = match kind {
        // `input` keeps its name. It is listed so a read of it counts as a
        // read of the contract, which is what hoists it for a helper outside
        // `handle`.
        ScriptKind::Procedure => &[
            ("input", "input"),
            ("params", "ctx.params"),
            ("collection", "ctx.collection"),
        ],
        ScriptKind::Query => &[("params", "input"), ("collection", "ctx.collection")],
        ScriptKind::RecordEvent => &[
            ("event", "input"),
            ("record", "input.record"),
            ("action", "input.action"),
            ("uri", "input.uri"),
            ("did", "input.did"),
            ("collection", "input.collection"),
            ("rkey", "input.rkey"),
        ],
        ScriptKind::Label => &[
            ("event", "input"),
            ("src", "input.src"),
            ("uri", "input.uri"),
            ("val", "input.val"),
            ("neg", "input.neg"),
            ("cts", "input.cts"),
            ("exp", "input.exp"),
        ],
        ScriptKind::Job | ScriptKind::Unknown => &[],
    };
    table
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)
}

/// `handle`'s declaration, kept until the whole file has been read: whether
/// its parameters can stay in place depends on where the script reads `input`
/// and `ctx`, which is not known until then.
struct HandleSite {
    /// The declaration's own name, where a marker about the signature goes.
    name_at: usize,
    parentheses: (usize, usize),
    parameters: Vec<String>,
    body: (usize, usize),
    /// Where a statement inserted at the top of the body goes, and the
    /// indentation to write it at. `None` for a body with no statements.
    first_statement: Option<(usize, String)>,
    /// Whether the body already opens with the hoist's own
    /// `input, ctx = handle_input, handle_ctx`.
    assigns_hoisted: bool,
}

struct Planner<'a> {
    source: &'a str,
    kind: ScriptKind,
    bindings: Bindings,
    /// Module names the script binds to something of its own. A rewrite that
    /// would need one of these is refused rather than shadowed.
    blocked: HashSet<&'static str>,
    /// Why `input` and `ctx` cannot be written here at all, when the script's
    /// `handle` is not a signature this rewrite can adapt.
    contract_refused: Option<&'static str>,
    linked_repo_handles: HashSet<String>,
    space_handles: HashSet<String>,
    blob_handles: HashSet<String>,
    /// Names bound to a page of space records.
    space_pages: HashSet<String>,
    /// Start bytes of the calls that stand alone as statements, whose result
    /// nothing can read.
    statement_calls: HashSet<usize>,
    reads_spaces_query: bool,
    edits: Vec<Edit>,
    notes: Vec<Note>,
    needed: BTreeSet<&'static str>,
    /// Shims the script needs, by the global each stands in for.
    polyfills: BTreeSet<&'static str>,
    /// Canonical module names whose reads went through a shim, which is what
    /// each shim's header names.
    shimmed: BTreeSet<&'static str>,
    handle: Option<HandleSite>,
    /// Where the rewrite reads `input` or `ctx`. A read outside `handle`'s own
    /// body is what the hoist exists for.
    contract_sites: Vec<usize>,
    /// Every function body's byte range. A read of `input` or `ctx` inside
    /// none of them runs as the chunk loads, which is before `handle` is
    /// called and so before either has a value, hoisted or not.
    function_bodies: Vec<(usize, usize)>,
    /// Marks are collected rather than placed as they are found: where the
    /// comment can go depends on the splices the rest of the pass produces.
    markers: Vec<(usize, String)>,
    /// Every statement's byte range, innermost last-resort anchors for a
    /// comment whose own line is inside a rewritten expression.
    statements: Vec<(usize, usize)>,
}

impl<'a> Planner<'a> {
    fn new(source: &'a str, ast: &Ast, kind: ScriptKind) -> Self {
        let bindings = bindings(ast);
        let required = requires::already_required(ast);
        let mut blocked: HashSet<&'static str> = requires::MODULES
            .iter()
            .filter(|m| bindings.names.contains(m.name) && !required.contains(m.name))
            .map(|m| m.name)
            .collect();
        let mut scan = ContextScan {
            linked_repos: HashSet::new(),
            spaces: HashSet::new(),
            blobs: HashSet::new(),
            pages: HashSet::new(),
            spaces_query: false,
            handle_parameters: None,
            handle_assigns_hoisted: false,
        };
        scan.visit_ast(ast);
        // `input`, `ctx` and `handle` are the names the rewrite itself puts in
        // scope. A script that binds one of them for its own purposes would
        // silently capture every rewrite that reads it — unless the binding is
        // the hoist's own declaration or assignment, which this rewrite wrote.
        let hoisted = hoist_in_place(ast) || scan.handle_assigns_hoisted;
        blocked.extend(INTRODUCED.iter().copied().filter(|name| {
            bindings.shadowed.contains(*name) && !(hoisted && matches!(*name, "input" | "ctx"))
        }));
        // A signature the rewrite cannot adapt is settled before any use is
        // visited, so a rewrite that would read `input` or `ctx` is refused
        // where it stands rather than emitted into a body that cannot see it.
        let contract_refused = match &scan.handle_parameters {
            None => Some(MARK_NO_HANDLE),
            Some(names) if names.len() > 2 => Some(MARK_HANDLE_PARAMS),
            Some(_) => None,
        };
        Planner {
            source,
            kind,
            bindings,
            blocked,
            contract_refused,
            linked_repo_handles: scan.linked_repos,
            space_handles: scan.spaces,
            blob_handles: scan.blobs,
            space_pages: scan.pages,
            statement_calls: HashSet::new(),
            reads_spaces_query: scan.spaces_query,
            edits: Vec::new(),
            notes: Vec::new(),
            needed: BTreeSet::new(),
            polyfills: BTreeSet::new(),
            shimmed: BTreeSet::new(),
            handle: None,
            contract_sites: Vec::new(),
            function_bodies: Vec::new(),
            markers: Vec::new(),
            statements: Vec::new(),
        }
    }

    // -- output ----------------------------------------------------------

    /// Take `edits` only if every name they depend on is available: the
    /// modules they call into, and whatever `input`/`ctx` they read. Answers
    /// whether they were taken, since a shim is needed on the same terms.
    fn commit(&mut self, path: &Path<'_>, modules: &[&'static str], edits: Vec<Edit>) -> bool {
        let introduced: Vec<&'static str> = edits
            .iter()
            .filter_map(|edit| match edit.pieces.first() {
                Some(Piece::Text(text)) => introduced_name(text),
                _ => None,
            })
            .collect();
        let reads_contract = introduced.iter().any(|name| *name != "handle");
        if reads_contract && let Some(message) = self.contract_refused {
            // A signature the declaration already carries a marker for is
            // one problem, not one per read; only a missing `handle` has no
            // declaration to carry it.
            if message == MARK_NO_HANDLE {
                self.mark(path, message.to_string());
            }
            return false;
        }
        if reads_contract
            && let Some(at) = path.start()
            && !self
                .function_bodies
                .iter()
                .any(|(start, end)| *start <= at && at < *end)
        {
            self.mark_at(at, MARK_READ_AT_LOAD.to_string());
            return false;
        }
        let taken = modules
            .iter()
            .copied()
            .chain(introduced)
            .find(|name| self.blocked.contains(*name));
        if let Some(name) = taken {
            if let Some(at) = path.start() {
                self.mark_at(at, blocked_message(name));
            }
            return false;
        }
        if reads_contract && let Some(at) = path.start() {
            self.contract_sites.push(at);
        }
        for module in modules {
            self.needed.insert(module);
        }
        self.edits.extend(edits);
        true
    }

    // -- handle ----------------------------------------------------------

    /// Adapt `handle`'s signature, and answer whether `input` and `ctx` have
    /// to be hoisted to file scope.
    ///
    /// v2 delivered context as file-wide globals, so a helper declared beside
    /// `handle` reading one of them is ordinary v2 code. Rewritten in place it
    /// would read `handle`'s parameters from outside `handle`, which is `nil`
    /// at run time and an error nowhere near the line that caused it.
    fn plan_handle(&mut self, ast: &Ast) -> bool {
        let Some(site) = self.handle.take() else {
            return false;
        };
        let names: Vec<&str> = site.parameters.iter().map(String::as_str).collect();
        let (body_start, body_end) = site.body;
        let outside = self
            .contract_sites
            .iter()
            .any(|at| *at < body_start || *at >= body_end);
        let declared = hoist_in_place(ast);
        let already_hoisted = names == HOISTED_PARAMS;
        // A signature this rewrite writes is left alone only while nothing
        // outside the body reads `input` or `ctx`, and the hoisted one only
        // while its declaration is still there: a helper added since the last
        // run, or a declaration removed by hand, would otherwise leave reads
        // of two undefined globals under a signature that reads as finished.
        if names == CONTRACT_PARAMS && !outside {
            return false;
        }
        if already_hoisted && !outside && declared {
            return false;
        }
        if names.len() > 2 {
            self.mark_at(site.name_at, MARK_HANDLE_PARAMS.to_string());
            return false;
        }
        if let Some(taken) = INTRODUCED
            .iter()
            .copied()
            .find(|name| self.blocked.contains(*name))
        {
            self.mark_at(site.name_at, blocked_message(taken));
            return false;
        }
        let hoist = outside || already_hoisted;
        let parameters = if hoist {
            HOISTED_PARAMS
        } else {
            CONTRACT_PARAMS
        };
        if hoist
            && !already_hoisted
            && let Some(taken) = parameters
                .iter()
                .copied()
                .find(|name| self.bindings.names.contains(*name))
        {
            self.mark_at(site.name_at, blocked_message(taken));
            return false;
        }

        if !already_hoisted {
            // A parameter list of the script's own naming still carries the
            // two values the contract names, so its uses are renamed rather
            // than left reading a parameter that is gone.
            for (index, old) in site.parameters.iter().enumerate() {
                let new = CONTRACT_PARAMS[index];
                if old == new {
                    continue;
                }
                if self.bindings.shadowed.contains(old) {
                    self.mark_at(site.name_at, blocked_message(old));
                    return false;
                }
                let mut uses = ParameterUses {
                    name: old,
                    within: site.body,
                    found: Vec::new(),
                };
                uses.visit_ast(ast);
                for (start, end) in uses.found {
                    self.edits.push(Edit::replace(start, end, new));
                }
            }

            let (open, close) = site.parentheses;
            self.edits.push(Edit::replace(
                open,
                close,
                format!("({}, {})", parameters[0], parameters[1]),
            ));
        }
        if hoist && !site.assigns_hoisted {
            let assignment = format!(
                "{}, {} = {}, {}",
                CONTRACT_PARAMS[0], CONTRACT_PARAMS[1], parameters[0], parameters[1]
            );
            let edit = match site.first_statement {
                Some((at, indent)) => Edit::insert(at, format!("{indent}{assignment}\n")),
                None => Edit::insert(site.parentheses.1, format!(" {assignment}")),
            };
            self.edits.push(edit);
        }
        hoist && !declared
    }

    fn mark(&mut self, path: &Path<'_>, message: String) {
        let Some(at) = path.start() else {
            return;
        };
        self.mark_at(at, message);
    }

    fn mark_at(&mut self, at: usize, message: String) {
        self.markers.push((at, message));
    }

    /// Turn the collected marks into comment lines, once every splice is
    /// known. A comment is anchored to the statement that encloses what it is
    /// about, so it cannot land inside an expression that is being replaced —
    /// where the splice would swallow it and leave a note with nothing to show
    /// for it in the source.
    fn place_markers(&mut self) {
        let replacements: Vec<(usize, usize)> = self
            .edits
            .iter()
            .filter(|edit| edit.start < edit.end)
            .map(|edit| (edit.start, edit.end))
            .collect();
        let mut markers = std::mem::take(&mut self.markers);
        markers.sort_by_key(|(at, _)| *at);
        let mut placed: HashSet<(usize, String)> = HashSet::new();
        for (at, message) in markers {
            let anchor = self.anchor(at, &replacements);
            if !placed.insert((anchor, message.clone())) {
                continue;
            }
            self.notes.push(Note {
                line: self.source[..at].matches('\n').count() + 1,
                message: message.clone(),
            });
            // A marker already standing above this statement is the one an
            // earlier pass wrote; re-stating it would grow the file every run.
            // Several can stack on one statement, so the whole run of them is
            // what counts as "above".
            if self
                .standing_markers(anchor)
                .any(|line| line == format!("-- codemod: {message}"))
            {
                continue;
            }
            let indent: String = self.source[anchor..]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            self.edits.push(Edit::insert(
                anchor,
                format!("{indent}-- codemod: {message}\n"),
            ));
        }
    }

    /// Where a comment about the construct at `at` can go: the start of the
    /// innermost enclosing statement's line, widening outwards until the point
    /// is not inside a replacement.
    fn anchor(&self, at: usize, replacements: &[(usize, usize)]) -> usize {
        let covered = |point: usize| {
            replacements
                .iter()
                .any(|(start, end)| *start < point && point < *end)
        };
        let mut enclosing: Vec<(usize, usize)> = self
            .statements
            .iter()
            .filter(|(start, end)| *start <= at && at <= *end)
            .copied()
            .collect();
        enclosing.sort_by_key(|(start, end)| end - start);
        let mut last = None;
        for (start, _) in enclosing {
            let point = self.line_start(start);
            if !covered(point) {
                return point;
            }
            last = Some(point);
        }
        last.unwrap_or_else(|| self.line_start(at))
    }

    fn line_start(&self, at: usize) -> usize {
        self.source[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    /// The `-- codemod:` lines directly above the line starting at
    /// `line_start`, nearest first, trimmed.
    fn standing_markers(&self, line_start: usize) -> impl Iterator<Item = &str> {
        self.source[..line_start]
            .lines()
            .rev()
            .map(str::trim)
            .take_while(|line| line.starts_with("-- codemod:"))
    }

    fn rename_head(&mut self, path: &Path<'_>, dots: usize, text: &str, modules: &[&'static str]) {
        let edits = match (path.start(), path.dot_end(dots)) {
            (Some(start), Some(end)) => vec![Edit::replace(start, end, text)],
            _ => return,
        };
        self.commit(path, modules, edits);
    }

    /// Replace the head, its dotted names and the call that follows them —
    /// everything up to and including `path.segs[at]`. Answers whether the
    /// replacement was taken.
    fn replace_through(
        &mut self,
        path: &Path<'_>,
        at: usize,
        pieces: Vec<Piece>,
        modules: &[&'static str],
    ) -> bool {
        let end = match path.segs.get(at) {
            Some(Seg::Call(args)) => end_of(*args),
            Some(Seg::Method(_, args)) => end_of(*args),
            _ => None,
        };
        let (Some(start), Some(end)) = (path.start(), end) else {
            return false;
        };
        self.commit(path, modules, vec![Edit { start, end, pieces }])
    }

    /// A read whose v3 answer is an envelope where v2 answered the body with
    /// `uri` on it. The replacement passes the result through the flatten
    /// shim, which is written once the first such read lands.
    fn flatten_through(
        &mut self,
        path: &Path<'_>,
        at: usize,
        pieces: Vec<Piece>,
        modules: &[&'static str],
    ) {
        if self.replace_through(path, at, pieces, modules) {
            self.polyfills.insert(polyfills::FLATTEN.name);
            self.shimmed.extend(modules);
        }
    }

    /// Whether a `db.get` or `db.search` call's result is read at all. One
    /// standing alone as a statement has no result to shape, so no wrapper is
    /// written around it, and `db.search`'s could not be: neither a table
    /// constructor nor a parenthesised expression is a statement. The chain
    /// reads wrap unconditionally, since a call on a chain is an expression
    /// either way.
    fn result_is_read(&self, path: &Path<'_>) -> bool {
        !self
            .statement_calls
            .contains(&path.start().unwrap_or(usize::MAX))
    }

    fn no_equivalent(&mut self, path: &Path<'_>, name: &str, module: Option<&'static str>) {
        let dotted = std::iter::once(name)
            .chain(path.dots())
            .collect::<Vec<_>>()
            .join(".");
        let advice = match module.and_then(requires::module_path) {
            Some(path) => format!("rewrite it using require(\"{path}\")"),
            None => "rewrite it by hand".to_string(),
        };
        self.mark(
            path,
            format!("{dotted} has no mechanical equivalent -- {advice}"),
        );
    }

    // -- dispatch --------------------------------------------------------

    fn visit_path(&mut self, path: Path<'_>) {
        for seg in &path.segs {
            match seg {
                // Only a space handle's record listing is renamed; a script's
                // own object with a `query` method is not this rewrite's
                // business.
                Seg::Method(name, FunctionArgs::TableConstructor(_))
                    if identifier(name) == Some("query") =>
                {
                    let on_a_space = path
                        .head_name()
                        .is_some_and(|head| self.space_handles.contains(head));
                    if on_a_space && let (Some(start), Some(end)) = (start_of(*name), end_of(*name))
                    {
                        self.edits.push(Edit::replace(start, end, "records"));
                    }
                }
                Seg::Dot(name) if self.reads_spaces_query => {
                    if identifier(name) == Some("authorDid")
                        && let Some(at) = start_of(*name)
                    {
                        self.mark_at(at, MARK_AUTHOR_DID.into());
                    }
                }
                _ => {}
            }
        }

        let Some(name) = path.head_name() else {
            return;
        };
        if self.bindings.in_scope(name, path.start().unwrap_or(0)) {
            if self.linked_repo_handles.contains(name)
                && let Some(Seg::Dot(field)) = path.segs.first()
                && identifier(field).is_some_and(|f| GRANT_FIELDS.contains(&f))
            {
                self.mark(&path, MARK_LINKED_FIELD.into());
            }
            // A dotted call is a method reached through the handle's
            // metatable; only a bare field read finds nothing there.
            if self.space_handles.contains(name)
                && matches!(path.segs.first(), Some(Seg::Dot(_)))
                && !matches!(path.segs.get(1), Some(Seg::Call(_)))
            {
                self.mark(&path, MARK_SPACE_FIELD.into());
            }
            // A downloaded blob is bytes and a MIME type on both sides; the
            // two spellings differ, and the v3 name implies no userdata.
            if self.blob_handles.contains(name)
                && let Some(Seg::Dot(field)) = path.segs.first()
                && let Some(renamed) =
                    identifier(field).and_then(|f| BLOB_FIELDS.iter().find(|(from, _)| *from == f))
                && let (Some(start), Some(end)) = (start_of(*field), end_of(*field))
            {
                self.edits.push(Edit::replace(start, end, renamed.1));
            }
            return;
        }
        if !detect::REMOVED_GLOBALS.contains(&name) {
            return;
        }
        // The global here, the script's own name a few lines away. Rewriting
        // would leave the two meanings of one word side by side, so this is
        // the one place a person has to choose.
        // Every migrated `handle` binds `input`, so that name being bound says
        // nothing here; a binding of the script's own is refused in `commit`.
        if name != "input" && self.bindings.names.contains(name) {
            self.mark(&path, blocked_message(name));
            return;
        }
        self.visit_global(name, &path);
    }

    fn visit_global(&mut self, name: &str, path: &Path<'_>) {
        let dots = path.dots();
        match name {
            "caller_did" => self.rename_head(path, 0, "ctx.caller_did", &[]),
            "delegate_did" => self.rename_head(path, 0, "ctx.delegate_did", &[]),
            "method" => self.rename_head(path, 0, "ctx.method", &[]),
            "env" => self.rename_head(path, 0, "ctx.env", &[]),
            "space" => match dots.first() {
                Some(&"space") => self.rename_head(path, 1, "ctx.space.uri", &[]),
                Some(&"space_id") => self.rename_head(path, 1, "ctx.space.id", &[]),
                Some(&"type_nsid") => self.rename_head(path, 1, "ctx.space.spaceType", &[]),
                _ => self.rename_head(path, 0, "ctx.space", &[]),
            },
            "job" => self.visit_job(path, &dots),
            "log" => self.visit_log(path),
            "now" => self.visit_now(path),
            "TID" => self.visit_tid(path),
            "json" => {
                self.commit(path, &["json"], Vec::new());
            }
            "toarray" => match path.segs.first() {
                Some(Seg::Call(_)) => self.rename_head(path, 0, "json.to_array", &["json"]),
                _ => self.no_equivalent(path, name, Some("json")),
            },
            "http" => {
                self.commit(path, &["http"], Vec::new());
            }
            "xrpc" => match dots.first() {
                Some(&"query") | Some(&"procedure") => {
                    self.need_polyfill(path, &polyfills::XRPC);
                }
                _ => self.no_equivalent(path, name, Some("xrpc")),
            },
            "jobs" => match dots.first() {
                Some(&"create") => {
                    self.commit(path, &["jobs"], Vec::new());
                }
                _ => self.no_equivalent(path, name, Some("jobs")),
            },
            "linked_repos" => match dots.first() {
                Some(&"list") | Some(&"get") => {
                    self.commit(path, &["linked_repos"], Vec::new());
                }
                _ => self.no_equivalent(path, name, Some("linked_repos")),
            },
            "db" => self.visit_db(path, &dots),
            "Record" => self.visit_record(path, &dots),
            "atproto" => self.visit_atproto(path, &dots),
            _ => self.visit_kind_global(name, path),
        }
    }

    fn visit_kind_global(&mut self, name: &str, path: &Path<'_>) {
        match kind_global(self.kind, name) {
            Some(text) => self.rename_head(path, 0, text, &[]),
            None if self.kind == ScriptKind::Unknown => self.mark(
                path,
                format!(
                    "{name} depends on the script's trigger kind -- re-run the codemod with that kind, or rewrite it by hand"
                ),
            ),
            None => self.mark(
                path,
                format!(
                    "{name} is not a global in a {} script -- rewrite it by hand",
                    self.kind.as_str()
                ),
            ),
        }
    }

    // -- built-ins -------------------------------------------------------

    fn visit_log(&mut self, path: &Path<'_>) {
        match path.segs.first() {
            Some(Seg::Call(_)) => self.rename_head(path, 0, "log.info", &["log"]),
            _ => self.no_equivalent(path, "log", Some("log")),
        }
    }

    fn visit_now(&mut self, path: &Path<'_>) {
        let Some(Seg::Call(args)) = path.segs.first() else {
            return self.no_equivalent(path, "now", Some("time"));
        };
        if !arguments(args).is_empty() {
            return self.no_equivalent(path, "now", Some("time"));
        }
        self.replace_through(
            path,
            0,
            vec![Piece::Text("time.to_iso8601(time.now())".into())],
            &["time"],
        );
    }

    /// Minting a TID is the only thing `TID` did that carries over. Every
    /// conversion on it speaks a different precision from `tids`/`time` —
    /// microseconds one side, milliseconds the other — so each is a marker
    /// rather than a rename.
    fn visit_tid(&mut self, path: &Path<'_>) {
        let Some(Seg::Call(args)) = path.segs.first() else {
            return self.no_equivalent(path, "TID", None);
        };
        if !arguments(args).is_empty() {
            return self.no_equivalent(path, "TID", Some("tids"));
        }
        self.replace_through(
            path,
            0,
            vec![Piece::Text("tids.create()".into())],
            &["tids"],
        );
    }

    fn visit_job(&mut self, path: &Path<'_>, dots: &[&str]) {
        if self.kind != ScriptKind::Job {
            return self.visit_kind_global("job", path);
        }
        match dots.first() {
            Some(&"input") => self.rename_head(path, 1, "input", &[]),
            Some(&"id") | Some(&"progress") | Some(&"should_stop") | Some(&"wait") => {
                self.rename_head(path, 0, "ctx.job", &[])
            }
            Some(&"log") => self.rename_head(path, 1, "log.info", &["log"]),
            Some(&"warn") => self.rename_head(path, 1, "log.warn", &["log"]),
            _ => self.no_equivalent(path, "job", None),
        }
    }

    // -- happyview.db ----------------------------------------------------

    fn visit_db(&mut self, path: &Path<'_>, dots: &[&str]) {
        match dots.first() {
            Some(&"backend") => {
                self.commit(path, &["db"], Vec::new());
            }
            Some(&"get") => self.visit_db_get(path),
            Some(&"raw") => self.rename_head(path, 1, "sql.raw", &["sql"]),
            Some(&"count") => self.visit_db_count(path),
            Some(&"search") => self.visit_db_search(path),
            Some(&"query") => self.visit_db_query(path),
            Some(&"backlinks") => self.visit_db_backlinks(path),
            _ => self.no_equivalent(path, "db", Some("db")),
        }
    }

    /// The argument list is carried over as source, so whatever is rewritten
    /// inside it comes with it.
    fn visit_db_get(&mut self, path: &Path<'_>) {
        let Some(Seg::Call(args)) = path.segs.get(1) else {
            return self.no_equivalent(path, "db", Some("db"));
        };
        if !self.result_is_read(path) {
            self.commit(path, &["db"], Vec::new());
            return;
        }
        let (Some(start), Some(end)) = (start_of(*args), end_of(*args)) else {
            return;
        };
        self.flatten_through(
            path,
            1,
            vec![
                Piece::Text("__codemod_flat(db.get".into()),
                Piece::Source(start, end),
                Piece::Text(")".into()),
            ],
            &["db"],
        );
    }

    fn visit_db_count(&mut self, path: &Path<'_>) {
        let Some(Seg::Call(args)) = path.segs.get(1) else {
            return self.no_equivalent(path, "db", Some("db"));
        };
        let mut pieces = vec![Piece::Text("db.records(".into())];
        match arguments(args).as_slice() {
            [collection] => match source_piece(collection) {
                Some(collection) => pieces.push(collection),
                None => return self.no_equivalent(path, "db", Some("db")),
            },
            [collection, did] => match (source_piece(collection), source_piece(did)) {
                (Some(collection), Some(did)) => {
                    pieces.push(collection);
                    pieces.push(Piece::Text("):did(".into()));
                    pieces.push(did);
                }
                _ => return self.no_equivalent(path, "db", Some("db")),
            },
            _ => return self.no_equivalent(path, "db", Some("db")),
        }
        pieces.push(Piece::Text("):count()".into()));
        self.replace_through(path, 1, pieces, &["db"]);
    }

    fn visit_db_search(&mut self, path: &Path<'_>) {
        let Some(table) = path.segs.get(1).and_then(call_table) else {
            return self.mark(path, MARK_DB_SEARCH.into());
        };
        let Some(options) = name_keys(table) else {
            return self.mark(path, MARK_DB_SEARCH.into());
        };
        let mut positional = Vec::new();
        for key in ["collection", "field", "query"] {
            let Some(value) = take(&options, key).and_then(source_piece) else {
                return self.mark(path, MARK_DB_SEARCH.into());
            };
            positional.push(value);
        }
        if let Some(limit) = take(&options, "limit") {
            let Some(limit) = source_piece(limit) else {
                return self.mark(path, MARK_DB_SEARCH.into());
            };
            positional.push(limit);
        }
        if options
            .iter()
            .any(|(key, _)| !["collection", "field", "query", "limit"].contains(key))
        {
            return self.mark(path, MARK_DB_SEARCH.into());
        }
        // v2 answered with `{records = ...}` and the library answers with the
        // array itself, so the wrapper is what keeps a caller's `.records`
        // working. It is parenthesised because Lua cannot index a table
        // constructor directly.
        let wrapped = self.result_is_read(path);
        let open = if wrapped {
            "({ records = __codemod_flat_rows(db.search("
        } else {
            "db.search("
        };
        let mut pieces = vec![Piece::Text(open.into())];
        for (index, argument) in positional.into_iter().enumerate() {
            if index > 0 {
                pieces.push(Piece::Text(", ".into()));
            }
            pieces.push(argument);
        }
        if wrapped {
            pieces.push(Piece::Text(")) })".into()));
            self.flatten_through(path, 1, pieces, &["db"]);
        } else {
            pieces.push(Piece::Text(")".into()));
            self.replace_through(path, 1, pieces, &["db"]);
        }
    }

    fn visit_db_query(&mut self, path: &Path<'_>) {
        const KNOWN: [&str; 7] = [
            "collection",
            "filter",
            "sort",
            "sortDirection",
            "limit",
            "cursor",
            "did",
        ];
        let Some(table) = path.segs.get(1).and_then(call_table) else {
            return self.mark(path, MARK_DB_QUERY.into());
        };
        let Some(options) = name_keys(table) else {
            return self.mark(path, MARK_DB_QUERY.into());
        };
        if options.iter().any(|(key, _)| !KNOWN.contains(key)) {
            return self.mark(path, MARK_DB_QUERY.into());
        }
        let Some(collection) = take(&options, "collection").and_then(source_piece) else {
            return self.mark(path, MARK_DB_QUERY.into());
        };

        let mut pieces = vec![
            Piece::Text("__codemod_flat_page(db.records(".into()),
            collection,
        ];
        pieces.push(Piece::Text(")".into()));

        if let Some(filter) = take(&options, "filter") {
            let Expression::TableConstructor(filter) = filter else {
                return self.mark(path, MARK_DB_QUERY.into());
            };
            let Some(condition) = name_keys(filter) else {
                return self.mark(path, MARK_DB_QUERY.into());
            };
            if condition
                .iter()
                .any(|(key, _)| !["field", "op", "value"].contains(key))
            {
                return self.mark(path, MARK_DB_QUERY.into());
            }
            let (Some(field), Some(value)) = (
                take(&condition, "field").and_then(source_piece),
                take(&condition, "value").and_then(source_piece),
            ) else {
                return self.mark(path, MARK_DB_QUERY.into());
            };
            let op = match take(&condition, "op") {
                Some(op) => match source_piece(op) {
                    Some(op) => op,
                    None => return self.mark(path, MARK_DB_QUERY.into()),
                },
                None => Piece::Text("\"=\"".into()),
            };
            pieces.push(Piece::Text(":where(".into()));
            pieces.push(field);
            pieces.push(Piece::Text(", ".into()));
            pieces.push(op);
            pieces.push(Piece::Text(", ".into()));
            pieces.push(value);
            pieces.push(Piece::Text(")".into()));
        }

        match (take(&options, "sort"), take(&options, "sortDirection")) {
            (Some(sort), direction) => {
                let Some(sort) = source_piece(sort) else {
                    return self.mark(path, MARK_DB_QUERY.into());
                };
                pieces.push(Piece::Text(":sort(".into()));
                pieces.push(sort);
                if let Some(direction) = direction {
                    let Some(direction) = source_piece(direction) else {
                        return self.mark(path, MARK_DB_QUERY.into());
                    };
                    pieces.push(Piece::Text(", ".into()));
                    pieces.push(direction);
                }
                pieces.push(Piece::Text(")".into()));
            }
            // A direction with nothing to sort by has no step to attach to.
            (None, Some(_)) => return self.mark(path, MARK_DB_QUERY.into()),
            (None, None) => {}
        }

        for key in ["limit", "cursor", "did"] {
            let Some(value) = take(&options, key) else {
                continue;
            };
            let Some(value) = source_piece(value) else {
                return self.mark(path, MARK_DB_QUERY.into());
            };
            pieces.push(Piece::Text(format!(":{key}(")));
            pieces.push(value);
            pieces.push(Piece::Text(")".into()));
        }
        pieces.push(Piece::Text(":run())".into()));
        self.flatten_through(path, 1, pieces, &["db"]);
    }

    fn visit_db_backlinks(&mut self, path: &Path<'_>) {
        const KNOWN: [&str; 5] = ["uri", "collection", "did", "limit", "cursor"];
        let Some(table) = path.segs.get(1).and_then(call_table) else {
            return self.mark(path, MARK_DB_BACKLINKS.into());
        };
        let Some(options) = name_keys(table) else {
            return self.mark(path, MARK_DB_BACKLINKS.into());
        };
        if options.iter().any(|(key, _)| !KNOWN.contains(key)) {
            return self.mark(path, MARK_DB_BACKLINKS.into());
        }
        let Some(uri) = take(&options, "uri").and_then(source_piece) else {
            return self.mark(path, MARK_DB_BACKLINKS.into());
        };
        let mut pieces = vec![Piece::Text("__codemod_flat_page(backlinks.to(".into()), uri];
        pieces.push(Piece::Text(")".into()));
        for key in ["collection", "did", "limit", "cursor"] {
            let Some(value) = take(&options, key) else {
                continue;
            };
            let Some(value) = source_piece(value) else {
                return self.mark(path, MARK_DB_BACKLINKS.into());
            };
            pieces.push(Piece::Text(format!(":{key}(")));
            pieces.push(value);
            pieces.push(Piece::Text(")".into()));
        }
        pieces.push(Piece::Text(":run())".into()));
        self.flatten_through(path, 1, pieces, &["backlinks"]);
    }

    // -- shims -----------------------------------------------------------

    /// A shim stands in for the whole global, so nothing at the call site
    /// changes: what the script needs is the block and the modules it reads.
    /// Answers whether the shim will be written.
    fn need_polyfill(&mut self, path: &Path<'_>, polyfill: &'static Polyfill) -> bool {
        let taken = self.commit(path, polyfill.modules, Vec::new());
        if taken {
            self.polyfills.insert(polyfill.name);
            self.shimmed.extend(polyfill.over);
        }
        taken
    }

    /// The statics the shim reproduces. Anything else on `Record` would read
    /// as nil off the shim.
    fn visit_record(&mut self, path: &Path<'_>, dots: &[&str]) {
        const STATICS: [&str; 5] = ["new", "load", "load_all", "save_all", "delete_local"];
        match dots.first() {
            None => {
                self.need_polyfill(path, &polyfills::RECORD);
            }
            Some(name) if STATICS.contains(name) => {
                self.need_polyfill(path, &polyfills::RECORD);
            }
            _ => self.no_equivalent(path, "Record", Some("record")),
        }
    }

    /// Whether an expression is a space handle: a v3 handle is methods on a
    /// metatable over an empty table, so handing one back answers nothing.
    fn is_space_handle(&self, expression: &Expression) -> bool {
        match expression {
            Expression::Var(Var::Name(name)) => {
                identifier(name).is_some_and(|name| self.space_handles.contains(name))
            }
            Expression::FunctionCall(call) => Path::build(call.prefix(), call.suffixes())
                .is_some_and(|path| space_handle_call(&path)),
            Expression::TableConstructor(table) => table
                .fields()
                .iter()
                .any(|field| field_value(field).is_some_and(|value| self.is_space_handle(value))),
            _ => false,
        }
    }

    /// Whether an expression is a page of space records: `authorDid` is
    /// renamed inside it, so handing it back as the response renames a key
    /// every caller sees.
    fn is_space_page(&self, expression: &Expression) -> bool {
        match expression {
            Expression::Var(Var::Name(name)) => {
                identifier(name).is_some_and(|name| self.space_pages.contains(name))
            }
            // `page.records` is the page unwrapped; its records still carry
            // the renamed key.
            Expression::Var(Var::Expression(var)) => {
                Path::build(var.prefix(), var.suffixes()).is_some_and(|path| {
                    path.head_name()
                        .is_some_and(|name| self.space_pages.contains(name))
                        && path.segs.len() == 1
                        && matches!(path.segs.first(), Some(Seg::Dot(field)) if identifier(field) == Some("records"))
                })
            }
            Expression::FunctionCall(call) => Path::build(call.prefix(), call.suffixes())
                .is_some_and(|path| space_page_call(&path, &self.space_handles)),
            Expression::TableConstructor(table) => table
                .fields()
                .iter()
                .any(|field| field_value(field).is_some_and(|value| self.is_space_page(value))),
            _ => false,
        }
    }

    // -- happyview.atproto and happyview.spaces --------------------------

    fn visit_atproto(&mut self, path: &Path<'_>, dots: &[&str]) {
        match dots.first() {
            Some(
                &"resolve_service_endpoint"
                | &"blob_download"
                | &"get_labels"
                | &"get_labels_batch"
                | &"sign"
                | &"verify_signature",
            ) => {
                self.commit(path, &["atproto"], Vec::new());
            }
            Some(&"blob_upload") => self.rename_head(path, 1, "record.upload_blob", &["record"]),
            Some(&"spaces") => match dots.get(1) {
                Some(&"get") => self.rename_head(path, 1, "spaces", &["spaces"]),
                Some(name @ (&"create" | &"accept_invite")) => self.visit_spaces_handle(path, name),
                Some(&"is_member") => self.visit_spaces_member(path, "is_member", 2),
                Some(&"get_access") => self.visit_spaces_member(path, "access", 2),
                Some(&"list_members") => self.visit_spaces_member(path, "members", 1),
                Some(&"query") => self.visit_spaces_query(path),
                _ => self.no_equivalent(path, "atproto", Some("spaces")),
            },
            _ => self.no_equivalent(path, "atproto", Some("atproto")),
        }
    }

    /// v2 answered with a handle carrying the space's methods; v3 answers with
    /// the space's fields, and the handle comes from `get`. The URI in those
    /// fields is what joins the two.
    fn visit_spaces_handle(&mut self, path: &Path<'_>, name: &str) {
        let Some(Seg::Call(args)) = path.segs.get(2) else {
            return self.no_equivalent(path, "atproto", Some("spaces"));
        };
        let (Some(start), Some(end)) = (start_of(*args), end_of(*args)) else {
            return self.no_equivalent(path, "atproto", Some("spaces"));
        };
        let mut edits = vec![Edit {
            start: match path.start() {
                Some(start) => start,
                None => return,
            },
            end,
            pieces: vec![
                Piece::Text(format!("spaces.get((spaces.{name}")),
                Piece::Source(start, end),
                Piece::Text(").uri)".into()),
            ],
        }];
        // The spec, the HTTP surface and the library all name a space's type
        // `spaceType`; a table carried forward verbatim would hand v3 a key
        // it does not read.
        if name == "create"
            && let Some(table) = path.segs.get(2).and_then(call_table)
        {
            for field in table.fields() {
                if let Field::NameKey { key, .. } = field
                    && identifier(key) == Some("type")
                    && let (Some(start), Some(end)) = (start_of(key), end_of(key))
                {
                    edits.push(Edit::replace(start, end, "spaceType"));
                }
            }
        }
        self.commit(path, &["spaces"], edits);
    }

    /// Membership is a handle method in v3 rather than a module function, so
    /// the space URI becomes the handle's argument and whatever followed it
    /// stays the method's.
    fn visit_spaces_member(&mut self, path: &Path<'_>, method: &str, arity: usize) {
        let Some(Seg::Call(args)) = path.segs.get(2) else {
            return self.no_equivalent(path, "atproto", Some("spaces"));
        };
        let arguments = arguments(args);
        if arguments.len() != arity {
            return self.no_equivalent(path, "atproto", Some("spaces"));
        }
        let Some(uri) = arguments.first().copied().and_then(source_piece) else {
            return self.no_equivalent(path, "atproto", Some("spaces"));
        };
        let mut pieces = vec![
            Piece::Text("spaces.get(".into()),
            uri,
            Piece::Text(format!("):{method}(")),
        ];
        for (index, argument) in arguments.iter().skip(1).enumerate() {
            let Some(argument) = source_piece(argument) else {
                return self.no_equivalent(path, "atproto", Some("spaces"));
            };
            if index > 0 {
                pieces.push(Piece::Text(", ".into()));
            }
            pieces.push(argument);
        }
        pieces.push(Piece::Text(")".into()));
        self.replace_through(path, 2, pieces, &["spaces"]);
    }

    fn visit_spaces_query(&mut self, path: &Path<'_>) {
        let mut edits: Vec<Edit> = match (path.start(), path.dot_end(1)) {
            (Some(start), Some(end)) => vec![Edit::replace(start, end, "spaces")],
            _ => return,
        };
        if let Some(table) = path.segs.get(2).and_then(call_table) {
            for field in table.fields() {
                if let Field::NameKey { key, .. } = field
                    && identifier(key) == Some("space_uri")
                    && let (Some(start), Some(end)) = (start_of(key), end_of(key))
                {
                    edits.push(Edit::replace(start, end, "uri"));
                }
            }
        }
        self.commit(path, &["spaces"], edits);
    }
}

impl Visitor for Planner<'_> {
    fn visit_var(&mut self, node: &Var) {
        match node {
            Var::Name(name) => self.visit_path(Path {
                head: name,
                segs: Vec::new(),
            }),
            Var::Expression(expression) => {
                if let Some(path) = Path::build(expression.prefix(), expression.suffixes()) {
                    self.visit_path(path);
                }
            }
            _ => {}
        }
    }

    fn visit_function_call(&mut self, node: &FunctionCall) {
        if let Some(path) = Path::build(node.prefix(), node.suffixes()) {
            self.visit_path(path);
        }
    }

    fn visit_function_body(&mut self, node: &full_moon::ast::FunctionBody) {
        if let Some(range) = span(node) {
            self.function_bodies.push(range);
        }
    }

    fn visit_function_declaration(&mut self, node: &full_moon::ast::FunctionDeclaration) {
        let Some(name) = handle_name(node) else {
            return;
        };
        let (open, close) = node.body().parameters_parentheses().tokens();
        let block = node.body().block();
        let (Some(name_at), Some(start), Some(end), Some(body_end)) = (
            start_of(name),
            start_of(open),
            end_of(close),
            start_of(node.body().end_token()),
        ) else {
            return;
        };
        // Only a statement that opens its own line can be pushed down by one:
        // anything else shares the line with the `function` keyword, and the
        // assignment goes straight after the parameters instead.
        let first = block
            .stmts()
            .next()
            .and_then(start_of)
            .or_else(|| block.last_stmt().and_then(start_of))
            .and_then(|at| {
                let line = self.line_start(at);
                let indent = &self.source[line..at];
                indent
                    .chars()
                    .all(|c| c == ' ' || c == '\t')
                    .then(|| (line, indent.to_string()))
            });
        self.handle = Some(HandleSite {
            name_at,
            parentheses: (start, end),
            parameters: parameter_names(node),
            body: (end, body_end),
            first_statement: first,
            assigns_hoisted: assigns_hoisted(block),
        });
    }

    /// A guard on a handle that v2 could hand back as nil. v3's `get` is a
    /// reference rather than a lookup and never fails, so the guard becomes
    /// dead code and the missing space or repo surfaces at the first method
    /// call instead.
    fn visit_expression(&mut self, node: &Expression) {
        let tested = match node {
            Expression::UnaryOperator {
                unop: UnOp::Not(_),
                expression,
                ..
            } => name_read(expression),
            Expression::BinaryOperator {
                lhs, binop, rhs, ..
            } => match binop {
                BinOp::And(_) => name_read(lhs),
                BinOp::TwoEqual(_) | BinOp::TildeEqual(_) => {
                    if is_nil(rhs) {
                        name_read(lhs)
                    } else if is_nil(lhs) {
                        name_read(rhs)
                    } else {
                        None
                    }
                }
                _ => None,
            },
            _ => None,
        };
        if let Some((name, at)) = tested
            && (self.space_handles.contains(name) || self.linked_repo_handles.contains(name))
        {
            self.mark_at(at, never_nil_message(name));
        }
    }

    fn visit_stmt(&mut self, node: &full_moon::ast::Stmt) {
        if let Some((start, end)) = span(node) {
            self.statements.push((start, end));
            if matches!(node, full_moon::ast::Stmt::FunctionCall(_)) {
                self.statement_calls.insert(start);
            }
        }
    }

    fn visit_last_stmt(&mut self, node: &LastStmt) {
        if let Some((start, end)) = span(node) {
            self.statements.push((start, end));
            if let LastStmt::Return(ret) = node {
                if ret.returns().iter().any(|value| self.is_space_page(value)) {
                    self.mark_at(start, MARK_AUTHOR_DID.into());
                }
                if ret
                    .returns()
                    .iter()
                    .any(|value| self.is_space_handle(value))
                {
                    self.mark_at(start, MARK_SPACE_RETURN.into());
                }
            }
        }
    }
}

fn field_value(field: &Field) -> Option<&Expression> {
    match field {
        Field::NameKey { value, .. } | Field::ExpressionKey { value, .. } | Field::NoKey(value) => {
            Some(value)
        }
        _ => None,
    }
}

/// A call that answers a space handle.
fn space_handle_call(path: &Path<'_>) -> bool {
    path.head_name() == Some("atproto")
        && matches!(
            path.dots().as_slice(),
            ["spaces", "get" | "create" | "accept_invite"]
        )
}

/// A call that answers a page of space records: a handle's own listing, or
/// the module's.
fn space_page_call(path: &Path<'_>, space_handles: &HashSet<String>) -> bool {
    let Some(head) = path.head_name() else {
        return false;
    };
    if space_handles.contains(head) {
        return matches!(path.segs.first(), Some(Seg::Method(name, _)) if identifier(name) == Some("query"));
    }
    head == "atproto" && path.dots() == ["spaces", "query"]
}

/// Whether a body opens with `input, ctx = handle_input, handle_ctx`.
fn assigns_hoisted(block: &Block) -> bool {
    let Some(Stmt::Assignment(assignment)) = block.stmts().next() else {
        return false;
    };
    let targets = assignment.variables().iter().map(|var| match var {
        Var::Name(name) => identifier(name),
        _ => None,
    });
    let values = assignment.expressions().iter().map(|value| match value {
        Expression::Var(Var::Name(name)) => identifier(name),
        _ => None,
    });
    targets.eq(CONTRACT_PARAMS.iter().map(|name| Some(*name)))
        && values.eq(HOISTED_PARAMS.iter().map(|name| Some(*name)))
}

fn span(node: &impl full_moon::node::Node) -> Option<(usize, usize)> {
    Some((start_of(node)?, end_of(node)?))
}

/// The declaration's name token when it declares the global `handle` the
/// runner calls, rather than a field or a method of the same name.
fn handle_name(
    node: &full_moon::ast::FunctionDeclaration,
) -> Option<&full_moon::tokenizer::TokenReference> {
    let mut names = node.name().names().iter();
    let first = names.next()?;
    if identifier(first) != Some("handle")
        || names.next().is_some()
        || node.name().method_name().is_some()
    {
        return None;
    }
    Some(first)
}

fn parameter_names(node: &full_moon::ast::FunctionDeclaration) -> Vec<String> {
    node.body()
        .parameters()
        .iter()
        .map(|parameter| match parameter {
            full_moon::ast::Parameter::Name(name) => identifier(name).unwrap_or("...").to_string(),
            _ => "...".to_string(),
        })
        .collect()
}

/// Whether the file already carries the hoist's own `local input, ctx`: two
/// names, no value, at the top level, which nothing but this rewrite writes.
fn hoist_in_place(ast: &Ast) -> bool {
    ast.nodes().stmts().any(|stmt| {
        let Stmt::LocalAssignment(local) = stmt else {
            return false;
        };
        local.expressions().is_empty()
            && local
                .names()
                .iter()
                .filter_map(identifier)
                .eq(CONTRACT_PARAMS.iter().copied())
    })
}

/// A bare name read, which is the only shape a nil test can be recognised in.
fn name_read(expression: &Expression) -> Option<(&str, usize)> {
    let Expression::Var(Var::Name(name)) = expression else {
        return None;
    };
    Some((identifier(name)?, start_of(name)?))
}

fn is_nil(expression: &Expression) -> bool {
    matches!(expression, Expression::Symbol(token) if token.token().to_string() == "nil")
}

/// Every read of one of `handle`'s parameters inside its body, so a renamed
/// parameter list does not leave the body reading a name that is gone.
struct ParameterUses<'a> {
    name: &'a str,
    within: (usize, usize),
    found: Vec<(usize, usize)>,
}

impl ParameterUses<'_> {
    fn record(&mut self, token: &full_moon::tokenizer::TokenReference) {
        let (Some(start), Some(end)) = (start_of(token), end_of(token)) else {
            return;
        };
        if identifier(token) == Some(self.name) && self.within.0 <= start && end <= self.within.1 {
            self.found.push((start, end));
        }
    }
}

impl Visitor for ParameterUses<'_> {
    fn visit_var(&mut self, node: &Var) {
        match node {
            Var::Name(name) => self.record(name),
            Var::Expression(expression) => {
                if let full_moon::ast::Prefix::Name(name) = expression.prefix() {
                    self.record(name);
                }
            }
            _ => {}
        }
    }

    fn visit_function_call(&mut self, node: &FunctionCall) {
        if let full_moon::ast::Prefix::Name(name) = node.prefix() {
            self.record(name);
        }
    }
}

/// The locals a script binds to a handle of some kind, whether it reads a
/// space's records at all, and what `handle` takes. All of them are facts
/// about the whole script that a rule needs before it reaches the line it
/// applies to.
struct ContextScan {
    linked_repos: HashSet<String>,
    spaces: HashSet<String>,
    blobs: HashSet<String>,
    pages: HashSet<String>,
    spaces_query: bool,
    handle_parameters: Option<Vec<String>>,
    handle_assigns_hoisted: bool,
}

impl Visitor for ContextScan {
    fn visit_local_assignment(&mut self, node: &full_moon::ast::LocalAssignment) {
        for (name, value) in node.names().iter().zip(node.expressions()) {
            let Expression::FunctionCall(call) = value else {
                continue;
            };
            let (Some(path), Some(name)) = (
                Path::build(call.prefix(), call.suffixes()),
                identifier(name),
            ) else {
                continue;
            };
            if space_page_call(&path, &self.spaces) {
                self.pages.insert(name.to_string());
                continue;
            }
            let dots = path.dots();
            match path.head_name() {
                Some("linked_repos") if dots.first() == Some(&"get") => {
                    self.linked_repos.insert(name.to_string());
                }
                Some("atproto") if dots.first() == Some(&"blob_download") => {
                    self.blobs.insert(name.to_string());
                }
                Some("atproto") if space_handle_call(&path) => {
                    self.spaces.insert(name.to_string());
                }
                _ => {}
            }
        }
    }

    fn visit_function_call(&mut self, node: &FunctionCall) {
        if let Some(path) = Path::build(node.prefix(), node.suffixes())
            && path.head_name() == Some("atproto")
            && path.dots() == ["spaces", "query"]
        {
            self.spaces_query = true;
        }
    }

    fn visit_function_declaration(&mut self, node: &full_moon::ast::FunctionDeclaration) {
        if handle_name(node).is_some() {
            self.handle_parameters = Some(parameter_names(node));
            self.handle_assigns_hoisted = assigns_hoisted(node.body().block());
        }
    }
}

fn call_table<'a>(seg: &'a Seg<'a>) -> Option<&'a TableConstructor> {
    let Seg::Call(args) = seg else { return None };
    match args {
        FunctionArgs::TableConstructor(table) => Some(table),
        FunctionArgs::Parentheses { .. } => match sole_argument(args)? {
            Expression::TableConstructor(table) => Some(table),
            _ => None,
        },
        _ => None,
    }
}

/// A table's `name = value` fields, or `None` if it holds anything else —
/// a positional entry or a computed key is outside every row of the table.
fn name_keys(table: &TableConstructor) -> Option<Vec<(&str, &Expression)>> {
    table
        .fields()
        .iter()
        .map(|field| match field {
            Field::NameKey { key, value, .. } => Some((identifier(key)?, value)),
            _ => None,
        })
        .collect()
}

fn take<'a>(options: &[(&str, &'a Expression)], key: &str) -> Option<&'a Expression> {
    options
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

fn sole_argument(args: &FunctionArgs) -> Option<&Expression> {
    match arguments(args).as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

fn arguments(args: &FunctionArgs) -> Vec<&Expression> {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => arguments.iter().collect(),
        _ => Vec::new(),
    }
}

fn source_piece(expression: &Expression) -> Option<Piece> {
    Some(Piece::Source(start_of(expression)?, end_of(expression)?))
}

#[cfg(test)]
mod tests {
    use super::ScriptKind;
    use super::rewrite;

    fn query(source: &str) -> String {
        rewrite(source, ScriptKind::Query).unwrap().source
    }

    #[test]
    fn a_db_query_with_no_collection_cannot_start_a_chain() {
        let result = rewrite(
            "function handle()\n  return db.query({ limit = 5 })\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
        assert!(result.source.contains("db.query({ limit = 5 })"));
    }

    #[test]
    fn a_sort_direction_with_nothing_to_sort_by_is_marked() {
        let result = rewrite(
            "function handle()\n  return db.query({ collection = \"c\", sort_direction = \"asc\" })\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
    }

    #[test]
    fn a_taken_module_name_stops_the_rewrite_that_needs_it() {
        let result = rewrite(
            "function handle()\n  local time = 5\n  return { a = now(), b = now(), t = time }\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert!(
            !result.source.contains("local time = require"),
            "{}",
            result.source
        );
        assert_eq!(result.notes.len(), 1);
        assert!(
            result.notes[0]
                .message
                .contains("'time' is bound by this script")
        );
    }

    #[test]
    fn only_a_global_handle_gains_the_new_parameters() {
        let source = "local m = {}\nfunction m.handle()\n  return 1\nend\n";
        assert_eq!(query(source), source);
    }

    #[test]
    fn a_record_use_carries_the_shim_rather_than_a_rename() {
        let result = rewrite(
            "function handle()\n  local r = Record.load(\"at://x/c/1\")\n  r.title = \"hi\"\n  return r:save()\nend\n",
            ScriptKind::Procedure,
        )
        .unwrap();
        assert!(result.notes.is_empty(), "{:?}", result.notes);
        assert!(
            result.source.contains("local Record = (function()"),
            "{}",
            result.source
        );
        assert!(
            result
                .source
                .contains("local record = require(\"happyview.record\")"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("Record.load(\"at://x/c/1\")"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_record_static_the_shim_does_not_have_is_marked() {
        let result = rewrite(
            "function handle()\n  return Record.find(\"c\")\nend\n",
            ScriptKind::Procedure,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
        assert!(result.notes[0].message.contains("Record.find"));
    }

    #[test]
    fn record_new_carries_the_shim() {
        let result = rewrite(
            "function handle()\n  return Record.new(\"c\", {}):save()\nend\n",
            ScriptKind::Procedure,
        )
        .unwrap();
        assert!(result.notes.is_empty(), "{:?}", result.notes);
        assert!(result.source.contains("local Record = (function()"));
    }

    #[test]
    fn a_space_handle_returned_straight_from_its_call_is_marked() {
        for source in [
            "function handle()\n  return atproto.spaces.get(params.uri)\nend\n",
            "function handle()\n  return atproto.spaces.create{ type = \"t\", skey = \"k\" }\nend\n",
            "function handle()\n  return { space = atproto.spaces.accept_invite{ token = params.t } }\nend\n",
        ] {
            let result = rewrite(source, ScriptKind::Query).unwrap();
            assert_eq!(result.notes.len(), 1, "{source}: {:?}", result.notes);
            assert!(
                result.notes[0]
                    .message
                    .contains("returning one answers nothing"),
                "{source}: {:?}",
                result.notes
            );
        }
    }

    #[test]
    fn a_three_parameter_handle_is_one_finding() {
        let result = rewrite(
            "function handle(a, b, c)\n  return { did = caller_did, q = params.q }\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert_eq!(result.notes[0].line, 1);
    }

    #[test]
    fn an_xrpc_call_carries_the_shim_and_keeps_its_status_guard() {
        let result = query(
            "function handle()\n  local r = xrpc.query(\"a.b.c\", {})\n  if r.status ~= 200 then return nil end\n  return r.body\nend\n",
        );
        assert!(result.contains("local xrpc = (function()"), "{result}");
        assert!(
            result.contains("local xrpc = require(\"happyview.xrpc\")"),
            "{result}"
        );
        assert!(result.contains("r.status ~= 200"), "{result}");
    }

    #[test]
    fn every_tid_conversion_is_marked_now_that_none_is_exact() {
        let result = rewrite(
            "function handle()\n  return TID.fromISO8601(\"2026-04-19T15:30:00Z\")\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
        assert!(
            result.source.contains("TID.fromISO8601"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_context_read_outside_handle_hoists_input_and_ctx() {
        let result = query(
            "local function who()\n  return caller_did\nend\n\nfunction handle()\n  return who()\nend\n",
        );
        assert!(result.starts_with("local input, ctx\n\n"), "{result}");
        assert!(
            result.contains("function handle(handle_input, handle_ctx)"),
            "{result}"
        );
        assert!(
            result.contains("  input, ctx = handle_input, handle_ctx\n"),
            "{result}"
        );
        assert!(result.contains("return ctx.caller_did"), "{result}");
    }

    #[test]
    fn a_context_read_only_inside_handle_is_left_where_it_is() {
        let result = query("function handle()\n  return caller_did\nend\n");
        assert!(!result.contains("local input, ctx"), "{result}");
        assert!(result.contains("function handle(input, ctx)"), "{result}");
    }

    #[test]
    fn a_handle_that_names_its_parameter_something_else_keeps_reading_it() {
        let result = rewrite(
            "function handle(evt)\n  return { uri = evt.uri, did = caller_did }\nend\n",
            ScriptKind::RecordEvent,
        )
        .unwrap();
        assert!(
            result.source.contains("function handle(input, ctx)"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("uri = input.uri"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("did = ctx.caller_did"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_script_with_no_handle_gets_no_contract_rewrite() {
        let result = rewrite(
            "local function report()\n  return caller_did\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert!(
            result.notes[0].message.contains("declare function handle"),
            "{:?}",
            result.notes
        );
        assert!(
            result.source.contains("return caller_did"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_handle_with_more_than_two_parameters_is_marked() {
        let result = rewrite(
            "function handle(a, b, c)\n  return caller_did\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert!(
            result
                .notes
                .iter()
                .any(|note| note.message.contains("exactly (input, ctx)")),
            "{:?}",
            result.notes
        );
        assert!(
            result.source.contains("function handle(a, b, c)"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("return caller_did"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_downloaded_blobs_fields_are_renamed_where_it_is_uploaded() {
        let result = query(
            "function handle()\n  local dl = atproto.blob_download(\"did:plc:a\", \"bafy\")\n  return atproto.blob_upload(dl.handle, dl.mimeType)\nend\n",
        );
        assert!(
            result.contains("record.upload_blob(dl.bytes, dl.mime_type)"),
            "{result}"
        );
    }

    /// Each v2 space field answers under the name v3 gives it, and the type
    /// is the one the spec renamed.
    #[test]
    fn the_space_globals_become_their_ctx_space_fields() {
        let result = query(
            "function handle()\n  return { uri = space.space, id = space.space_id, kind = space.type_nsid, skey = space.skey }\nend\n",
        );
        assert!(
            result.contains(
                "{ uri = ctx.space.uri, id = ctx.space.id, kind = ctx.space.spaceType, skey = ctx.space.skey }"
            ),
            "{result}"
        );
    }

    #[test]
    fn a_created_space_is_fetched_back_as_a_handle() {
        let result = query(
            "function handle()\n  local s = atproto.spaces.create{ type = \"t\", skey = \"k\" }\n  return s:query{ limit = 1 }\nend\n",
        );
        assert!(
            result.contains("spaces.get((spaces.create{ spaceType = \"t\", skey = \"k\" }).uri)"),
            "{result}"
        );
        assert!(result.contains("s:records{ limit = 1 }"), "{result}");
    }

    #[test]
    fn a_nil_test_on_a_handle_that_can_no_longer_be_nil_is_marked() {
        let space = rewrite(
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  if not s then return nil end\n  return s:records{}\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert!(
            space.notes.iter().any(|note| note
                .message
                .contains("'s' is a handle and v3 handles are never nil")),
            "{:?}",
            space.notes
        );

        let repo = rewrite(
            "function handle()\n  local r = linked_repos.get(\"did:plc:a\")\n  if r == nil then return nil end\n  return true\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert!(
            repo.notes.iter().any(|note| note
                .message
                .contains("'r' is a handle and v3 handles are never nil")),
            "{:?}",
            repo.notes
        );
    }

    #[test]
    fn a_handle_already_on_the_contract_still_hoists_for_a_helper_outside_it() {
        let result = query(
            "local function who()\n  return caller_did\nend\n\nfunction handle(input, ctx)\n  return who()\nend\n",
        );
        assert!(result.starts_with("local input, ctx\n\n"), "{result}");
        assert!(
            result.contains("function handle(handle_input, handle_ctx)\n  input, ctx = handle_input, handle_ctx\n"),
            "{result}"
        );
        assert!(result.contains("return ctx.caller_did"), "{result}");
    }

    #[test]
    fn a_hoisted_signature_with_no_declaration_gets_one() {
        let result = query(
            "function handle(handle_input, handle_ctx)\n  input, ctx = handle_input, handle_ctx\n  return input.q\nend\n",
        );
        assert!(result.starts_with("local input, ctx\n\n"), "{result}");
        assert_eq!(
            result
                .matches("input, ctx = handle_input, handle_ctx")
                .count(),
            1,
            "{result}"
        );
        assert!(
            result.contains("function handle(handle_input, handle_ctx)"),
            "{result}"
        );
    }

    #[test]
    fn a_procedures_input_read_in_a_helper_hoists_like_any_context_read() {
        let source = "local function q()\n  return input.q\nend\n\nfunction handle()\n  return { q = q() }\nend\n";
        let result = rewrite(source, ScriptKind::Procedure).unwrap();
        assert!(result.notes.is_empty(), "{:?}", result.notes);
        assert_eq!(
            result.source,
            "local input, ctx\n\nlocal function q()\n  return input.q\nend\n\nfunction handle(handle_input, handle_ctx)\n  input, ctx = handle_input, handle_ctx\n  return { q = q() }\nend\n"
        );

        let again = rewrite(&result.source, ScriptKind::Procedure).unwrap();
        assert_eq!(again.source, result.source);
        assert!(again.notes.is_empty(), "{:?}", again.notes);
    }

    /// `handle(input, ctx)` binds `input` for its own body only, so the
    /// signature being in place does not make the helper's read a bound one.
    #[test]
    fn a_helpers_input_read_hoists_under_a_signature_already_on_the_contract() {
        let result = rewrite(
            "local function q()\n  return input.q\nend\n\nfunction handle(input, ctx)\n  return { q = q(), s = input.s }\nend\n",
            ScriptKind::Procedure,
        )
        .unwrap();
        assert!(result.notes.is_empty(), "{:?}", result.notes);
        assert!(
            result.source.starts_with("local input, ctx\n\n"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("function handle(handle_input, handle_ctx)\n  input, ctx = handle_input, handle_ctx\n"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_procedure_on_the_contract_rewrites_to_itself() {
        let source = "function handle(input, ctx)\n  return { status = input.status, who = ctx.caller_did }\nend\n";
        let result = rewrite(source, ScriptKind::Procedure).unwrap();
        assert_eq!(result.source, source);
        assert!(result.notes.is_empty(), "{:?}", result.notes);
    }

    /// A hoisted `input` is still `nil` while the chunk loads, so a read at
    /// file scope is left for a person rather than rewritten into one that
    /// fails on load.
    #[test]
    fn a_procedures_input_read_at_file_scope_is_marked_rather_than_hoisted() {
        let source =
            "local seed = input.seed\n\nfunction handle()\n  return { seed = seed }\nend\n";
        let result = rewrite(source, ScriptKind::Procedure).unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert_eq!(result.notes[0].line, 1);
        assert_eq!(result.notes[0].message, super::MARK_READ_AT_LOAD);
        assert!(
            !result.source.contains("local input, ctx"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("\nlocal seed = input.seed\n"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("function handle(input, ctx)"),
            "{}",
            result.source
        );

        let again = rewrite(&result.source, ScriptKind::Procedure).unwrap();
        assert_eq!(again.source, result.source);
        assert_eq!(again.notes.len(), 1, "{:?}", again.notes);
    }

    #[test]
    fn any_context_read_at_file_scope_is_marked() {
        let result = rewrite(
            "local BASE = env.API_URL\n\nfunction handle()\n  return { base = BASE, q = params.q }\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert_eq!(result.notes[0].message, super::MARK_READ_AT_LOAD);
        assert!(
            result.source.contains("local BASE = env.API_URL"),
            "{}",
            result.source
        );
        assert!(result.source.contains("q = input.q"), "{}", result.source);
    }

    #[test]
    fn a_free_input_is_not_a_global_outside_a_procedure() {
        let result = rewrite(
            "local function q()\n  return input.q\nend\n\nfunction handle()\n  return q()\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert!(
            result.notes[0]
                .message
                .contains("input is not a global in a query script"),
            "{:?}",
            result.notes
        );
    }

    #[test]
    fn a_hoisted_script_is_left_alone_on_a_second_run() {
        let source = "local input, ctx\n\nlocal function who()\n  return ctx.caller_did\nend\n\nfunction handle(handle_input, handle_ctx)\n  input, ctx = handle_input, handle_ctx\n  return who()\nend\n";
        let result = rewrite(source, ScriptKind::Query).unwrap();
        assert_eq!(result.source, source);
        assert!(result.notes.is_empty(), "{:?}", result.notes);
    }

    #[test]
    fn a_field_read_off_a_space_handle_is_marked_and_a_method_call_is_not() {
        let result = rewrite(
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  s:write_record{ collection = \"c\", record = {} }\n  return { uri = s.uri }\nend\n",
            ScriptKind::Procedure,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert!(
            result.notes[0]
                .message
                .contains("space handles carry no fields"),
            "{:?}",
            result.notes
        );
        assert_eq!(result.notes[0].line, 4);
    }

    #[test]
    fn a_db_get_is_read_through_the_flatten_shim() {
        let result = rewrite(
            "function handle()\n  return db.get(params.uri).title\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert!(result.notes.is_empty(), "{:?}", result.notes);
        assert!(
            result
                .source
                .contains("return __codemod_flat(db.get(input.uri)).title"),
            "{}",
            result.source
        );
        assert!(
            result
                .source
                .contains("-- codemod polyfill: v2 row shape over happyview.db;"),
            "{}",
            result.source
        );
        assert!(
            result
                .source
                .contains("local db = require(\"happyview.db\")"),
            "{}",
            result.source
        );
    }

    #[test]
    fn every_read_shape_passes_through_the_shim_written_once() {
        let result = query(
            "function handle()\n  local one = db.get(params.uri)\n  local page = db.query({ collection = \"c\" })\n  local found = db.search({ collection = \"c\", field = \"f\", query = \"q\" })\n  local likes = db.backlinks({ uri = params.uri })\n  return { one = one, page = page, found = found, likes = likes }\nend\n",
        );
        assert!(
            result.contains("__codemod_flat(db.get(input.uri))"),
            "{result}"
        );
        assert!(
            result.contains("__codemod_flat_page(db.records(\"c\"):run())"),
            "{result}"
        );
        assert!(
            result.contains("({ records = __codemod_flat_rows(db.search(\"c\", \"f\", \"q\")) })"),
            "{result}"
        );
        assert!(
            result.contains("__codemod_flat_page(backlinks.to(input.uri):run())"),
            "{result}"
        );
        assert_eq!(
            result.matches("-- codemod polyfill: v2 row shape").count(),
            1,
            "{result}"
        );
    }

    #[test]
    fn a_backlinks_read_alone_needs_no_db_require_for_the_shim() {
        let result = query("function handle()\n  return db.backlinks({ uri = params.uri })\nend\n");
        assert!(!result.contains("require(\"happyview.db\")"), "{result}");
        assert!(
            result.contains("local backlinks = require(\"happyview.backlinks\")"),
            "{result}"
        );
        assert!(
            result.contains("-- codemod polyfill: v2 row shape over happyview.backlinks;"),
            "{result}"
        );
    }

    #[test]
    fn a_bare_db_search_statement_writes_no_shim() {
        let result = query(
            "function handle()\n  db.search({ collection = \"c\", field = \"f\", query = \"q\" })\n  return {}\nend\n",
        );
        assert!(
            result.contains("  db.search(\"c\", \"f\", \"q\")\n"),
            "{result}"
        );
        assert!(!result.contains("__codemod_flat"), "{result}");
    }

    #[test]
    fn a_bare_db_get_statement_is_neither_wrapped_nor_shimmed() {
        let result = rewrite(
            "db.get(TID.toNumber(x))\n\nfunction handle()\n  return 1\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert!(
            result
                .source
                .contains("-- codemod: TID.toNumber has no mechanical equivalent -- rewrite it by hand\ndb.get(TID.toNumber(x))\n"),
            "{}",
            result.source
        );
        assert_eq!(result.source.matches("-- codemod:").count(), 1);
        assert!(
            !result.source.contains("__codemod_flat"),
            "{}",
            result.source
        );
        assert!(
            result
                .source
                .contains("local db = require(\"happyview.db\")"),
            "{}",
            result.source
        );
    }

    /// A marker anchored at the statement's first byte sits at the wrapped
    /// call's own start; the source piece begins after `db.get`, so the
    /// insert renders once, above the statement, and never inside the call.
    #[test]
    fn a_marker_at_the_start_of_a_wrapped_db_get_is_written_once() {
        let result = rewrite(
            "db.get(TID.toNumber(x)).seen = true\n\nfunction handle()\n  return 1\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert!(
            result
                .source
                .contains("-- codemod: TID.toNumber has no mechanical equivalent -- rewrite it by hand\n__codemod_flat(db.get(TID.toNumber(x))).seen = true\n"),
            "{}",
            result.source
        );
        assert_eq!(result.source.matches("-- codemod:").count(), 1);
    }

    #[test]
    fn the_row_shape_header_names_only_the_libraries_read_through() {
        let backlinks_only =
            query("function handle()\n  return db.backlinks({ uri = params.uri })\nend\n");
        assert!(
            backlinks_only.contains("-- codemod polyfill: v2 row shape over happyview.backlinks;"),
            "{backlinks_only}"
        );
        let both = query(
            "function handle()\n  return { a = db.get(params.uri), b = db.backlinks({ uri = params.uri }) }\nend\n",
        );
        assert!(
            both.contains(
                "-- codemod polyfill: v2 row shape over happyview.db and happyview.backlinks;"
            ),
            "{both}"
        );
    }

    #[test]
    fn a_db_get_read_as_a_value_is_marked() {
        let result = rewrite(
            "function handle()\n  local get = db.get\n  return get(params.uri)\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert!(
            result.notes[0]
                .message
                .starts_with("db.get has no mechanical equivalent"),
            "{:?}",
            result.notes
        );
        assert!(
            !result.source.contains("__codemod_flat"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_space_page_handed_straight_back_is_marked() {
        for source in [
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  return s:query{ limit = 1 }\nend\n",
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  local page = s:query{ limit = 1 }\n  return page\nend\n",
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  local page = s:query{ limit = 1 }\n  return { page = page }\nend\n",
            "function handle()\n  return atproto.spaces.query{ uri = \"at://x\" }\nend\n",
        ] {
            let result = rewrite(source, ScriptKind::Query).unwrap();
            assert_eq!(result.notes.len(), 1, "{source}: {:?}", result.notes);
            assert!(
                result.notes[0].message.contains("author_did"),
                "{source}: {:?}",
                result.notes
            );
        }
        let not_a_page = query(
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  return { members = s:members() }\nend\n",
        );
        assert!(!not_a_page.contains("-- codemod:"), "{not_a_page}");
    }

    #[test]
    fn an_unwrapped_space_page_is_marked_too() {
        for source in [
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  local page = s:query{ limit = 1 }\n  return page.records\nend\n",
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  local page = s:query{ limit = 1 }\n  return { records = page.records }\nend\n",
        ] {
            let result = rewrite(source, ScriptKind::Query).unwrap();
            assert_eq!(result.notes.len(), 1, "{source}: {:?}", result.notes);
            assert!(
                result.notes[0].message.contains("author_did"),
                "{source}: {:?}",
                result.notes
            );
        }
        let cursor_only = query(
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  local page = s:query{ limit = 1 }\n  return { cursor = page.cursor }\nend\n",
        );
        assert!(!cursor_only.contains("-- codemod:"), "{cursor_only}");
    }

    #[test]
    fn a_space_handle_handed_straight_back_is_marked() {
        for source in [
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  return s\nend\n",
            "function handle()\n  local s = atproto.spaces.create{ type = \"t\", skey = \"k\" }\n  return { space = s }\nend\n",
        ] {
            let result = rewrite(source, ScriptKind::Query).unwrap();
            assert_eq!(result.notes.len(), 1, "{source}: {:?}", result.notes);
            assert!(
                result.notes[0]
                    .message
                    .contains("returning one answers nothing"),
                "{source}: {:?}",
                result.notes
            );
        }
    }

    #[test]
    fn a_shim_refused_for_a_taken_name_is_not_written() {
        let result = rewrite(
            "function handle()\n  local record = {}\n  return Record.load(\"at://x/c/1\")\nend\n",
            ScriptKind::Procedure,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
        assert!(
            result.notes[0]
                .message
                .contains("'record' is bound by this script"),
            "{:?}",
            result.notes
        );
        assert!(!result.source.contains("local Record = (function()"));
    }

    #[test]
    fn job_controls_outside_a_job_script_are_marked() {
        let result = rewrite(
            "function handle()\n  return { id = job.id }\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
        assert!(result.notes[0].message.contains("not a global in a query"));
        assert!(result.source.contains("job.id"));
    }

    #[test]
    fn a_query_method_becomes_records_only_on_a_space_handle() {
        let on_a_space = query(
            "function handle()\n  local s = atproto.spaces.get(\"at://x\")\n  return s:query{ limit = 1 }\nend\n",
        );
        assert!(
            on_a_space.contains("s:records{ limit = 1 }"),
            "{on_a_space}"
        );

        let on_anything_else = query("function handle()\n  return s:query{ limit = 1 }\nend\n");
        assert!(
            on_anything_else.contains("s:query{ limit = 1 }"),
            "{on_anything_else}"
        );
    }

    #[test]
    fn a_marker_inside_a_rewritten_construct_still_reaches_the_source() {
        let result = rewrite(
            "function handle()\n  return db.query({\n    collection = \"c\",\n    limit = TID.toNumber(x),\n  })\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
        assert!(
            result.source.contains("db.records(\"c\")"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("-- codemod: TID.toNumber"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_script_that_binds_input_gets_no_contract_rewrite() {
        let result = rewrite(
            "function handle()\n  local input = {}\n  return params.q\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 2, "{:?}", result.notes);
        assert!(
            result
                .notes
                .iter()
                .all(|note| note.message.contains("'input' is bound by this script")),
            "{:?}",
            result.notes
        );
        assert!(
            result.source.contains("function handle()"),
            "{}",
            result.source
        );
        assert!(
            result.source.contains("return params.q"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_script_that_binds_ctx_gets_no_contract_rewrite() {
        let result = rewrite(
            "function handle()\n  local ctx = {}\n  return caller_did\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert!(
            result.notes[0]
                .message
                .contains("'ctx' is bound by this script"),
            "{:?}",
            result.notes
        );
        assert!(
            result.source.contains("return caller_did"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_global_the_script_also_binds_is_marked_rather_than_rewritten() {
        let result = rewrite(
            "function handle()\n  local rows = db.get(\"at://x\")\n  for _, db in ipairs(rows) do end\n  return rows\nend\n",
            ScriptKind::Query,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
        assert!(
            result.notes[0]
                .message
                .contains("'db' is bound by this script")
        );
        assert!(
            !result.source.contains("require(\"happyview.db\")"),
            "{}",
            result.source
        );
    }

    #[test]
    fn a_name_used_only_as_the_scripts_own_local_is_left_alone() {
        let source = "function handle(input, ctx)\n  local record = { title = \"hi\" }\n  return record.title\nend\n";
        let result = rewrite(source, ScriptKind::RecordEvent).unwrap();
        assert_eq!(result.source, source);
        assert!(result.notes.is_empty(), "{:?}", result.notes);
    }

    #[test]
    fn a_use_after_a_loop_variable_goes_out_of_scope_is_the_global_again() {
        let result = rewrite(
            "function handle()\n  for _, uri in ipairs({}) do end\n  return uri\nend\n",
            ScriptKind::RecordEvent,
        )
        .unwrap();
        assert_eq!(result.notes.len(), 1);
        assert!(
            result.notes[0]
                .message
                .contains("'uri' is bound by this script")
        );
    }

    #[test]
    fn db_search_keeps_the_result_shape_v2_returned() {
        let result = query(
            "function handle()\n  return db.search({ collection = \"c\", field = \"f\", query = \"q\" }).records\nend\n",
        );
        assert!(
            result.contains(
                "({ records = __codemod_flat_rows(db.search(\"c\", \"f\", \"q\")) }).records"
            ),
            "{result}"
        );
    }

    #[test]
    fn a_bracket_index_on_env_keeps_its_key_expression() {
        assert!(
            query("function handle()\n  return env[params.key]\nend\n")
                .contains("ctx.env[input.key]")
        );
    }

    #[test]
    fn a_marked_construct_still_gets_its_contract_rewrites() {
        let result = query(
            "function handle()\n  return db.query({ collection = \"c\", nope = caller_did })\nend\n",
        );
        assert!(result.contains("nope = ctx.caller_did"), "{result}");
        assert!(result.contains("-- codemod: db.query"), "{result}");
    }
}
