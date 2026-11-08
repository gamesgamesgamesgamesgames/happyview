//! Guardrails for raw SQL surfaces that let scripts and plugins talk to the
//! database directly: Lua's `db.raw` and the plugin host's `host_db_query` /
//! `host_db_execute` imports. Both share the same table guard so protected
//! internal tables cannot be reached from either surface.

/// Table-name prefix reserved for HappyView's own internal tables. `db.raw`
/// blocks these **by default** — so a table added in a future migration is
/// protected until it is deliberately allowed — except for the data tables in
/// [`ALLOWED_INTERNAL_TABLES`].
const PROTECTED_TABLE_PREFIX: &str = "happyview_";

/// Internal tables that don't carry the `happyview_` prefix but are still
/// off-limits (SQLx's migration bookkeeping).
const PROTECTED_EXACT_TABLES: &[&str] = &["_sqlx_migrations"];

/// Internal tables `db.raw` is allowed to read and write despite the reserved
/// prefix: public AppView data and background jobs. Everything else
/// `happyview_*` stays blocked — secrets and tokens (`happyview_dpop_keys`/
/// `_sessions`, `happyview_api_keys`, `happyview_oauth_*`,
/// `happyview_script_variables`), auth/privilege state
/// (`happyview_users`/`_user_permissions`, the delegation tables), trust config
/// (`happyview_domains`, `happyview_instance_settings`), cryptographic
/// material, and every space table.
///
/// Space tables are reached only through `happyview.spaces`, which applies the
/// space's access rules for its caller. Raw SQL reads and writes rows
/// directly, outside those rules.
const ALLOWED_INTERNAL_TABLES: &[&str] = &[
    // Public AppView data.
    "happyview_records",
    "happyview_record_refs",
    "happyview_records_by_collection",
    "happyview_labels",
    crate::catalog::lexicon_rows::TABLE,
    // Background jobs.
    "happyview_jobs",
];

/// Reject a raw SQL string that references a protected internal table.
///
/// Tokenizes the SQL (so string literals and comments containing the prefix are
/// ignored, and quoted / schema-qualified identifiers are still caught) and
/// blocks any `happyview_*` (or `_sqlx_migrations`) identifier that is not in
/// [`ALLOWED_INTERNAL_TABLES`]. Unicode-escaped identifiers (`U&"…"`) are refused
/// outright as an evasion vector, and SQL that cannot be tokenized fails closed.
pub fn check_raw_sql_tables(sql: &str) -> Result<(), String> {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    // `U&'…'` / `U&"…"` unicode-escaped literals could smuggle a protected
    // identifier past tokenization (the escapes decode to letters); there is no
    // legitimate need for them in raw SQL, so refuse them outright.
    let lowered = sql.to_ascii_lowercase();
    if lowered.contains("u&\"") || lowered.contains("u&'") {
        return Err("raw SQL does not allow unicode-escaped identifiers".into());
    }

    // Tokenizing (rather than substring matching) means the prefix inside string
    // literals or comments is ignored, while quoted and schema-qualified
    // identifiers are still seen. SQL we cannot tokenize fails closed.
    let tokens = Tokenizer::new(&GenericDialect {}, sql)
        .tokenize()
        .map_err(|e| format!("could not parse SQL: {e}"))?;

    for token in tokens {
        if let Token::Word(word) = token {
            let name = word.value.to_ascii_lowercase();
            let is_internal = name.starts_with(PROTECTED_TABLE_PREFIX)
                || PROTECTED_EXACT_TABLES.contains(&name.as_str());
            if is_internal && !ALLOWED_INTERNAL_TABLES.contains(&name.as_str()) {
                return Err(format!(
                    "raw SQL cannot reference the protected internal HappyView table '{}'",
                    word.value
                ));
            }
        }
    }

    Ok(())
}

/// The internal relations a plugin's raw SQL may name, and the relations the
/// least-privileged Postgres roles are granted. One source of truth so the
/// parser allowlist in [`check_postgres_plugin_sql`] and the role grants in
/// `plugin::host::db` cannot drift apart.
pub fn plugin_nameable_relations() -> &'static [&'static str] {
    ALLOWED_INTERNAL_TABLES
}

/// Whether a plugin holds only `database:read` or `database:write`. Chooses the
/// statement kinds the parser allowlist accepts and, on Postgres, the role the
/// statement runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginSqlMode {
    Read,
    Write,
}

/// True when `name` is one of HappyView's own internal tables that plugin raw
/// SQL may not reach — the same rule as [`check_raw_sql_tables`], but applied to
/// a name the real parser extracted. A caller-owned table (anything without the
/// reserved prefix) is not blocked; plugins are expected to use their own
/// tables through raw SQL.
fn is_blocked_internal_table(name: &str) -> bool {
    let lname = name.to_ascii_lowercase();
    let internal = lname.starts_with(PROTECTED_TABLE_PREFIX)
        || PROTECTED_EXACT_TABLES.contains(&lname.as_str());
    internal
        && !ALLOWED_INTERNAL_TABLES
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&lname))
}

/// Functions that change session state and so must be refused in **either**
/// mode and whether or not the roles are set up — the function form of
/// `SET`/`RESET`. Left in, `set_config('role', …)` / `set_config('search_path',
/// …)` would do from a statement what the refused `SET` statement cannot.
const ALWAYS_FORBIDDEN_FUNCTIONS: &[&str] = &["set_config"];

/// True when a write-mode statement is of a kind plugin raw SQL may run.
///
/// DML writes are always allowed. Table-shaped DDL (`CREATE`/`ALTER`/`DROP`
/// TABLE, INDEX, VIEW) is allowed only when `allow_ddl` — i.e. when the
/// least-privileged roles are in force. Without them the parser is the whole
/// boundary and DDL is refused outright: `pg_query`'s walker does not descend
/// into a column `DEFAULT`/`CHECK`/`GENERATED` expression, so a function hidden
/// there would escape the fallback function check; forbidding DDL in the
/// fallback closes that without a bespoke walker. Under the roles those
/// expressions run as the restricted role and are contained there.
///
/// This is an **allowlist of statement kinds**, so an unlisted or future kind
/// fails closed. It leaves out every role/session/privilege and code-defining
/// kind — `DO`, `CREATE FUNCTION`/`OPERATOR`/`CAST`/`AGGREGATE`/`TRIGGER`/
/// `RULE`/`EXTENSION`/`LANGUAGE`/`EVENT TRIGGER`, `COPY`, `EXECUTE`/`PREPARE`,
/// `GRANT`/`REVOKE`, the role statements, `SET`/`RESET`, and transaction
/// control — which is how the CVE-2018-1058 object-shadowing paths are closed.
/// `ALTER TABLE … OWNER TO` is refused even though `ALTER TABLE` is allowed.
fn is_allowed_write_statement(
    node: Option<&pg_query::protobuf::node::Node>,
    allow_ddl: bool,
) -> bool {
    use pg_query::protobuf::node::Node;
    match node {
        Some(
            Node::SelectStmt(_)
            | Node::InsertStmt(_)
            | Node::UpdateStmt(_)
            | Node::DeleteStmt(_)
            | Node::MergeStmt(_),
        ) => true,
        Some(Node::CreateStmt(_) | Node::IndexStmt(_) | Node::ViewStmt(_) | Node::DropStmt(_)) => {
            allow_ddl
        }
        Some(Node::AlterTableStmt(s)) => allow_ddl && !alter_table_changes_owner(s),
        _ => false,
    }
}

/// True when an `ALTER TABLE` includes an `OWNER TO` sub-command. Changing a
/// table's owner is a privilege operation, refused even though the rest of
/// `ALTER TABLE` is allowed.
fn alter_table_changes_owner(stmt: &pg_query::protobuf::AlterTableStmt) -> bool {
    use pg_query::protobuf::node::Node;
    // `AlterTableType::AtChangeOwner` is 28.
    const AT_CHANGE_OWNER: i32 = 28;
    stmt.cmds.iter().any(|cmd| {
        matches!(
            cmd.node.as_ref(),
            Some(Node::AlterTableCmd(c)) if c.subtype == AT_CHANGE_OWNER || c.newowner.is_some()
        )
    })
}

/// Functions plugin raw SQL may call on Postgres **when the least-privileged
/// roles are not available** and the parser is the whole boundary. An
/// **allowlist**: every function the real parser sees must be here, so a
/// string-executing built-in (`query_to_xml`, `table_to_xml`, `dblink*`,
/// `pg_read_file`, …) is refused by default rather than hunted for in a
/// deny-list. Operators and `CAST`/`::` are not function calls in the parse
/// tree, so they are allowed without listing; the risk in a cast is never the
/// cast but the function that consumes its result, which this list gates.
///
/// When the roles *are* available this list is not consulted: the role, which
/// has no grant on the protected tables, denies such a read at the database, so
/// a plugin keeps full use of functions over its own tables. Extend it on
/// request when a plugin needs a demonstrably-safe function in the fallback;
/// keep anything that reads files, runs a string as SQL, opens a connection, or
/// touches server state out of it.
const PLUGIN_SQL_FUNCTIONS: &[&str] = &[
    // Aggregates.
    "count",
    "sum",
    "avg",
    "min",
    "max",
    // Null / conditional helpers that do parse as calls.
    "coalesce",
    "nullif",
    "greatest",
    "least",
    // Strings.
    "lower",
    "upper",
    "length",
    "char_length",
    "trim",
    "btrim",
    "ltrim",
    "rtrim",
    "substr",
    "substring",
    "left",
    "right",
    "replace",
    "concat",
    "concat_ws",
    "split_part",
    "position",
    "strpos",
    "starts_with",
    "format",
    "md5",
    "encode",
    "regexp_replace",
    // Numbers.
    "abs",
    "round",
    "ceil",
    "ceiling",
    "floor",
    "mod",
    "power",
    "sign",
    // Time.
    "now",
    "statement_timestamp",
    "clock_timestamp",
    "date_trunc",
    "to_timestamp",
    "to_char",
    "to_date",
    "extract",
    "date_part",
    "age",
    "make_timestamptz",
    // Window ranking.
    "row_number",
    "rank",
    "dense_rank",
    // JSON (read/build only — no execution).
    "json_extract_path_text",
    "jsonb_extract_path_text",
    "json_array_length",
    "jsonb_array_length",
    "json_build_object",
    "jsonb_build_object",
    "json_build_array",
    "jsonb_build_array",
    "to_json",
    "to_jsonb",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "jsonb_object_keys",
    "array_agg",
    "string_agg",
    "jsonb_agg",
    "json_agg",
    "unnest",
];

/// Guard plugin raw SQL **on Postgres**, using Postgres's own parser
/// (libpg_query via `pg_query`) rather than a tokenizer, so SQL hidden in a
/// string handed to a built-in is seen. Applied on every Postgres plugin
/// statement; it is the whole boundary when the least-privileged roles are not
/// available (`roles_available == false`).
///
/// Always enforced:
/// - **Relations.** No statement may reference one of HappyView's own internal
///   tables except the allowlisted data ones (`plugin_nameable_relations`).
///   Caller-owned tables (any name without the reserved prefix) are allowed, so
///   a plugin keeps using its own tables and DDL on them.
/// - **Statement kinds.** Read mode permits only `SELECT`; write mode permits
///   the DML writes and table-shaped DDL (`is_allowed_write_statement`). This
///   refuses `DO`, `CREATE FUNCTION`, `COPY`, `EXECUTE`, `GRANT`, the role
///   statements, transaction control and — load-bearing — `SET`/`RESET`, in
///   both modes.
///
/// Enforced only in the fallback (`roles_available == false`), where the parser
/// is the only boundary:
/// - **Functions.** Every function called must be on [`PLUGIN_SQL_FUNCTIONS`],
///   so a string-executing built-in that would read a protected table from a
///   string literal is refused. When the roles are available this is left to the
///   role, which the database denies the protected read, so a plugin keeps full
///   use of functions over its own tables.
///
/// SQL that does not parse fails closed. This does not by itself prove a read is
/// side-effect-free (a data-modifying CTE, a locking clause, `SELECT … INTO`);
/// the caller gates read-mode SQL with [`is_read_only`] as well.
pub fn check_postgres_plugin_sql(
    sql: &str,
    mode: PluginSqlMode,
    roles_available: bool,
) -> Result<(), String> {
    use pg_query::protobuf::node::Node;

    let parsed = pg_query::parse(sql).map_err(|e| format!("could not parse SQL: {e}"))?;

    if parsed.protobuf.stmts.is_empty() {
        return Err("raw SQL contained no statement".into());
    }

    // Statement kinds. DDL (caller-owned table management) is allowed only when
    // the roles are in force; the fallback is DML-only (see
    // `is_allowed_write_statement`).
    let allow_ddl = roles_available;
    for raw in &parsed.protobuf.stmts {
        let node = raw.stmt.as_ref().and_then(|n| n.node.as_ref());
        let allowed = match mode {
            PluginSqlMode::Read => matches!(node, Some(Node::SelectStmt(_))),
            PluginSqlMode::Write => is_allowed_write_statement(node, allow_ddl),
        };
        if !allowed {
            return Err(match mode {
                PluginSqlMode::Read => {
                    "database:read permits only read-only SELECT statements in raw SQL".to_string()
                }
                PluginSqlMode::Write => {
                    "raw SQL permits data statements and table DDL, not this statement kind"
                        .to_string()
                }
            });
        }
    }

    // Every relation and function reference in the whole parse tree. The scan
    // is generic (it walks the serialized tree), so it sees references in a CTE,
    // a scalar subquery, a `RETURNING` list, an `ON CONFLICT` update and a
    // `LATERAL` subquery alike — anywhere a reference can hide — not only the
    // positions `pg_query`'s own `tables()`/`functions()` descend into.
    let (relations, functions) = scan_relations_and_functions(&parsed);

    // Relations: refuse HappyView's own internal tables (except the allowlisted
    // data ones). Caller-owned tables pass.
    for table in &relations {
        let (_schema, name) = split_qualified(table);
        if is_blocked_internal_table(name) {
            return Err(format!(
                "raw SQL cannot reference the protected internal HappyView table '{table}'"
            ));
        }
    }

    // Functions. `set_config` and friends are the function form of SET and are
    // refused in either mode. The rest of the allowlist is the whole boundary
    // only when the roles are unavailable (the DML-only fallback). With the
    // roles, a string-executing built-in is left to the role, which the database
    // denies, so a plugin keeps full use of functions over its own tables.
    for func in &functions {
        let (schema, name) = split_qualified(func);
        if ALWAYS_FORBIDDEN_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(name))
        {
            return Err(format!("raw SQL cannot call the function '{func}'"));
        }
        if !roles_available {
            if let Some(schema) = schema
                && !(schema.eq_ignore_ascii_case("public")
                    || schema.eq_ignore_ascii_case("pg_catalog"))
            {
                return Err(format!(
                    "raw SQL cannot call '{func}' (schema '{schema}' is not allowed)"
                ));
            }
            if !PLUGIN_SQL_FUNCTIONS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(name))
            {
                return Err(format!(
                    "raw SQL cannot call the function '{func}' unless the plugin SQL database roles are configured; it is not on the fallback allowlist"
                ));
            }
        }
    }

    Ok(())
}

/// Collect every relation and function reference in the parse tree, as
/// `"schema.name"` (or bare `name`). Generic over the whole serialized tree so
/// no reference position is missed; see the caller for why that matters. The
/// parse tree is bounded by the plugin's statement size, so the one allocation
/// is cheap relative to the database round trip it guards.
fn scan_relations_and_functions(parsed: &pg_query::ParseResult) -> (Vec<String>, Vec<String>) {
    let mut relations = Vec::new();
    let mut functions = Vec::new();
    if let Ok(tree) = serde_json::to_value(&parsed.protobuf) {
        collect_refs(&tree, &mut relations, &mut functions);
    }
    (relations, functions)
}

/// Walk a serialized parse tree, collecting every relation and function
/// reference (both possibly schema-qualified).
///
/// A relation reference is any object carrying a `relname` string: a `RangeVar`
/// appears both tagged inside a `Node` (a `FROM` item) and inline as a typed
/// struct field (`UpdateStmt.relation`, `DeleteStmt.relation`, …), and both
/// shapes have `relname`. A function reference is any object carrying a
/// `funcname` array (a `FuncCall`), whose entries are `String` nodes nested
/// under a `node` wrapper.
fn collect_refs(
    value: &serde_json::Value,
    relations: &mut Vec<String>,
    functions: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(rel) = map.get("relname").and_then(|v| v.as_str()) {
                let schema = map
                    .get("schemaname")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                relations.push(match schema {
                    Some(s) => format!("{s}.{rel}"),
                    None => rel.to_string(),
                });
            }
            if let Some(name) = qualified_name_from_string_nodes(map.get("funcname")) {
                functions.push(name);
            }
            for child in map.values() {
                collect_refs(child, relations, functions);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_refs(child, relations, functions);
            }
        }
        _ => {}
    }
}

/// Turn a `funcname` array of `String` nodes
/// (`[{"node":{"String":{"sval":"lower"}}}]`, or schema-qualified
/// `[{…"pg_catalog"}, {…"set_config"}]`) into `"schema.name"` or bare `name`,
/// keeping the last two parts.
fn qualified_name_from_string_nodes(funcname: Option<&serde_json::Value>) -> Option<String> {
    let parts: Vec<&str> = funcname?
        .as_array()?
        .iter()
        .filter_map(|n| n.get("node")?.get("String")?.get("sval")?.as_str())
        .collect();
    match parts.as_slice() {
        [] => None,
        [name] => Some((*name).to_string()),
        [.., schema, name] => Some(format!("{schema}.{name}")),
    }
}

/// Split a possibly schema-qualified name from `pg_query`'s `tables()` /
/// `functions()` (`"public.happyview_records"`) into `(schema, name)`. Only the
/// last two dot-separated parts matter; a catalog-qualified name keeps its
/// schema so the caller can reject a non-`public` one.
fn split_qualified(qualified: &str) -> (Option<&str>, &str) {
    match qualified.rsplit_once('.') {
        Some((schema, name)) => (Some(schema), name),
        None => (None, qualified),
    }
}

/// Why a raw write to a claimed collection's records is refused.
pub const CLAIMED_WRITE_REFUSED: &str = "writes to records of a collection stored outside this instance must go through save_local in happyview.record";

/// The compat view raw SQL writes records through.
const RECORDS_VIEW: &str = "happyview_records";

/// With collections stored outside this instance (`claims` says which), refuse
/// a raw write to `happyview_records` that could create or change a local row
/// for one of them: reads of a claimed collection go to the external storage,
/// so the row would never be seen.
///
/// A write is checked by the collections it names: every string literal in
/// the statement and every string parameter, read as a collection NSID or a
/// record URI. One that is claimed refuses the write; a write naming no
/// collection at all cannot be checked, and is refused too. Read-only
/// statements, and statements that do not mention `happyview_records`, pass.
/// SQL that cannot be tokenized fails closed.
pub fn check_claimed_record_writes(
    sql: &str,
    params: &[serde_json::Value],
    claims: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    if is_read_only(sql) == Ok(true) {
        return Ok(());
    }
    let tokens = Tokenizer::new(&GenericDialect {}, sql)
        .tokenize()
        .map_err(|e| format!("could not parse SQL: {e}"))?;
    let writes_records = tokens.iter().any(
        |token| matches!(token, Token::Word(word) if word.value.eq_ignore_ascii_case(RECORDS_VIEW)),
    );
    if !writes_records {
        return Ok(());
    }
    let literals = tokens.iter().filter_map(|token| match token {
        Token::SingleQuotedString(s)
        | Token::EscapedStringLiteral(s)
        | Token::NationalStringLiteral(s) => Some(s.as_str()),
        Token::DollarQuotedString(s) => Some(s.value.as_str()),
        _ => None,
    });
    let params = params.iter().filter_map(serde_json::Value::as_str);
    let mut named = false;
    for collection in literals.chain(params).filter_map(named_collection) {
        if claims(collection) {
            return Err(format!("{CLAIMED_WRITE_REFUSED}: {collection}"));
        }
        named = true;
    }
    if named {
        Ok(())
    } else {
        Err(format!(
            "a write to {RECORDS_VIEW} must name its collection or record URI as a literal \
             or parameter while collections are stored outside this instance"
        ))
    }
}

/// The collection `text` names: the collection of a record URI, or `text`
/// itself when it has an NSID's shape.
fn named_collection(text: &str) -> Option<&str> {
    if text.starts_with("at://") {
        return crate::record_store::codec::parse_record_uri(text).map(|(_, c, _)| c);
    }
    let segments: Vec<&str> = text.split('.').collect();
    (segments.len() >= 3
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }))
    .then_some(text)
}

/// True when a `Query`'s body — and every CTE feeding it — is read-only.
///
/// sqlparser 0.62 parses `WITH x AS (...) DELETE/INSERT/UPDATE/MERGE ...` as a
/// `Statement::Query` whose `body` is a data-modifying `SetExpr` variant, not as
/// a `Statement::Delete`/`Insert`/etc — so checking the outer `Statement`
/// variant alone is not enough. A CTE's own body can independently be a write
/// (`WITH x AS (DELETE FROM t RETURNING uri) SELECT * FROM x`), so every
/// `with.cte_tables[].query` is checked recursively as well.
fn query_is_read_only(query: &sqlparser::ast::Query) -> bool {
    use sqlparser::ast::SetExpr;

    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            if !query_is_read_only(&cte.query) {
                return false;
            }
        }
    }

    fn set_expr_is_read_only(expr: &SetExpr) -> bool {
        match expr {
            // `SELECT .. INTO` creates a table; both it and locking clauses write.
            SetExpr::Select(select) => select.into.is_none(),
            SetExpr::Values(_) | SetExpr::Table(_) => true,
            SetExpr::Query(q) => query_is_read_only(q),
            SetExpr::SetOperation { left, right, .. } => {
                set_expr_is_read_only(left) && set_expr_is_read_only(right)
            }
            SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => {
                false
            }
        }
    }

    query.locks.is_empty() && set_expr_is_read_only(&query.body)
}

/// True when every statement is a query whose body and CTEs are all
/// read-only. Parsed with the generic dialect and fails closed: SQL that
/// does not parse is not known to be read-only.
pub fn is_read_only(sql: &str) -> Result<bool, String> {
    use sqlparser::ast::Statement;
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    let statements = Parser::parse_sql(&GenericDialect {}, sql)
        .map_err(|e| format!("could not parse SQL: {e}"))?;
    Ok(!statements.is_empty()
        && statements.iter().all(|s| match s {
            Statement::Query(q) => query_is_read_only(q) && !has_nested_locks(s),
            _ => false,
        }))
}

/// True when every statement is a write: a statement other than a query, or
/// a query whose body is an `INSERT`, `UPDATE`, `DELETE` or `MERGE`. A read
/// with a data-modifying CTE, a batch mixing reads and writes, and SQL that
/// does not parse are not.
pub fn is_write(sql: &str) -> bool {
    use sqlparser::ast::{SetExpr, Statement};
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    let Ok(statements) = Parser::parse_sql(&GenericDialect {}, sql) else {
        return false;
    };
    !statements.is_empty()
        && statements.iter().all(|s| match s {
            Statement::Query(q) => matches!(
                *q.body,
                SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_)
            ),
            _ => true,
        })
}

/// True when any query nested in the statement (a subquery, say) has a
/// locking clause (`FOR UPDATE` / `FOR SHARE`).
fn has_nested_locks(statement: &sqlparser::ast::Statement) -> bool {
    use sqlparser::ast::Visit;

    struct Locks;
    impl sqlparser::ast::Visitor for Locks {
        type Break = ();
        fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> std::ops::ControlFlow<()> {
            if query.locks.is_empty() {
                std::ops::ControlFlow::Continue(())
            } else {
                std::ops::ControlFlow::Break(())
            }
        }
    }
    statement.visit(&mut Locks).is_break()
}

/// Why a raw read that mixes record views with other relations is refused.
pub const RAW_MIXED_REFUSED: &str =
    "this query reads claimed records and local tables together; split it into two queries";

/// The compat views that serve record reads.
const RECORD_VIEWS: [&str; 3] = [
    "happyview_records",
    "happyview_record_refs",
    "happyview_records_by_collection",
];

/// True when `table`, an unquoted identifier, names a record view.
pub fn is_record_view(table: &str) -> bool {
    RECORD_VIEWS.contains(&table.to_ascii_lowercase().as_str())
}

/// Which relations a raw query reads, as far as the record views go.
#[derive(Debug, PartialEq, Eq)]
pub enum RecordViews {
    /// None of the record views.
    None,
    /// Only the record views.
    Only,
    /// The record views together with another relation, named here.
    Mixed(String),
}

/// Which relations a read-only raw query reads: none of the record compat
/// views, only them (CTE names and table functions don't count), or them
/// together with another relation. SQL that names a view but does not parse
/// fails closed, and so does anything not plainly a view: a name qualified
/// with a schema other than `main` or `public`, or a name that is not a plain
/// identifier, counts as another relation. So does a `WITH` defining more
/// than [`MAX_CTES`] CTEs, which is refused.
///
/// The check is syntactic, so it cannot see through a user function that reads
/// the views (`my_func()`), or a user-created view built on them. The caller
/// must gate the SQL with [`is_read_only`] first; writes are not classified.
pub fn record_view_use(sql: &str) -> Result<RecordViews, String> {
    use sqlparser::ast::Visit;
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    use sqlparser::tokenizer::{Token, Tokenizer};

    // Cheap pre-check on tokens, so SQL that never names a view is never parsed.
    let dialect = GenericDialect {};
    let names_view = match Tokenizer::new(&dialect, sql).tokenize() {
        Ok(tokens) => tokens.iter().any(|token| {
            matches!(token, Token::Word(word)
                if RECORD_VIEWS.contains(&word.value.to_ascii_lowercase().as_str()))
        }),
        Err(_) => {
            let lowered = sql.to_ascii_lowercase();
            RECORD_VIEWS.iter().any(|view| lowered.contains(view))
        }
    };
    if !names_view {
        return Ok(RecordViews::None);
    }

    let statements = Parser::parse_sql(&dialect, sql)
        .map_err(|e| format!("raw SQL that reads record views must parse: {e}"))?;
    let mut relations = Relations::default();
    for statement in &statements {
        let _ = statement.visit(&mut relations);
    }
    if relations.too_many_ctes {
        return Err(format!(
            "raw SQL that reads record views may define at most {MAX_CTES} CTEs in one WITH"
        ));
    }
    Ok(match relations.other {
        Some(name) => RecordViews::Mixed(name),
        None if relations.saw_view => RecordViews::Only,
        None => RecordViews::None,
    })
}

/// The most CTEs one `WITH` may define in SQL that reads the record views.
/// Each CTE body is checked against the ones before it, so the cost grows
/// with the square of the count; more fails closed.
const MAX_CTES: usize = 1000;

/// Schemas a record view may be qualified with.
const VIEW_SCHEMAS: [&str; 2] = ["main", "public"];

/// Collects the relations a query reads, resolving CTE names by scope.
#[derive(Default)]
struct Relations {
    /// The CTE names visible at the current point, one set per enclosing query.
    scopes: Vec<std::collections::HashSet<String>>,
    saw_view: bool,
    /// The first relation that is not a record view.
    other: Option<String>,
    /// CTE bodies already checked on their own; the main walk skips them.
    checked: std::collections::HashSet<*const sqlparser::ast::Query>,
    /// How many enclosing queries are being skipped.
    skipping: usize,
    /// A `WITH` defined more than [`MAX_CTES`] CTEs; the walk stopped there.
    too_many_ctes: bool,
}

/// An identifier as the database compares it: unquoted names fold to lower case.
fn fold(ident: &sqlparser::ast::Ident) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_ascii_lowercase()
    }
}

impl Relations {
    /// Record a relation read, given its optional schema and its name.
    fn record(&mut self, schema: Option<&str>, name: String) -> std::ops::ControlFlow<()> {
        if self.skipping > 0 {
            return std::ops::ControlFlow::Continue(());
        }
        let schema_ok = schema.is_none_or(|s| VIEW_SCHEMAS.contains(&s));
        // A CTE can only be named unqualified.
        if schema.is_none() && self.scopes.iter().any(|scope| scope.contains(&name)) {
            return std::ops::ControlFlow::Continue(());
        }
        if schema_ok && RECORD_VIEWS.contains(&name.as_str()) {
            self.saw_view = true;
            return std::ops::ControlFlow::Continue(());
        }
        self.other = Some(match schema {
            Some(schema) => format!("{schema}.{name}"),
            None => name,
        });
        std::ops::ControlFlow::Break(())
    }

    fn scan_body(&mut self, body: &sqlparser::ast::SetExpr) -> std::ops::ControlFlow<()> {
        use sqlparser::ast::SetExpr;
        match body {
            // `TABLE x` is a relation read that is not a `TableFactor`.
            SetExpr::Table(table) => {
                let schema = table.schema_name.as_deref().map(str::to_ascii_lowercase);
                match &table.table_name {
                    Some(name) => self.record(schema.as_deref(), name.to_ascii_lowercase()),
                    None => self.record(schema.as_deref(), "table".into()),
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.scan_body(left)?;
                self.scan_body(right)
            }
            _ => std::ops::ControlFlow::Continue(()),
        }
    }
}

impl sqlparser::ast::Visitor for Relations {
    type Break = ();

    fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> std::ops::ControlFlow<()> {
        use sqlparser::ast::Visit;

        // A body checked on its own is not walked again, which would make
        // nested CTEs cost exponential time.
        if self.skipping > 0 || self.checked.contains(&(query as *const _)) {
            self.skipping += 1;
            self.scopes.push(Default::default());
            return std::ops::ControlFlow::Continue(());
        }
        let mut scope = std::collections::HashSet::new();
        if let Some(with) = &query.with {
            if with.cte_tables.len() > MAX_CTES {
                self.too_many_ctes = true;
                return std::ops::ControlFlow::Break(());
            }
            // Without RECURSIVE, a CTE body sees only the CTEs before it, so a
            // name it shares with its own CTE or a later one is a real table.
            // The scope below covers every CTE, which is right for the main
            // body but would hide that; check each body on its own.
            if !with.recursive {
                for (i, cte) in with.cte_tables.iter().enumerate() {
                    let mut body = Relations {
                        scopes: self.scopes.clone(),
                        ..Relations::default()
                    };
                    body.scopes.push(
                        with.cte_tables[..i]
                            .iter()
                            .map(|c| fold(&c.alias.name))
                            .collect(),
                    );
                    let _ = cte.query.visit(&mut body);
                    self.checked.insert(&*cte.query as *const _);
                    self.saw_view |= body.saw_view;
                    if body.too_many_ctes {
                        self.too_many_ctes = true;
                        return std::ops::ControlFlow::Break(());
                    }
                    if let Some(name) = body.other {
                        self.other = Some(name);
                        return std::ops::ControlFlow::Break(());
                    }
                }
            }
            scope.extend(with.cte_tables.iter().map(|c| fold(&c.alias.name)));
        }
        self.scopes.push(scope);
        self.scan_body(&query.body)
    }

    fn post_visit_query(&mut self, _query: &sqlparser::ast::Query) -> std::ops::ControlFlow<()> {
        self.scopes.pop();
        self.skipping = self.skipping.saturating_sub(1);
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(
        &mut self,
        factor: &sqlparser::ast::TableFactor,
    ) -> std::ops::ControlFlow<()> {
        use sqlparser::ast::ObjectNamePart;
        // `args: Some(..)` is a table function (`json_each(..)`), not a relation.
        let sqlparser::ast::TableFactor::Table {
            name, args: None, ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let mut idents = Vec::new();
        for part in &name.0 {
            match part {
                ObjectNamePart::Identifier(ident) => idents.push(fold(ident)),
                // Not a plain name: fail closed.
                ObjectNamePart::Function(_) => return self.record(Some("?"), name.to_string()),
            }
        }
        let Some(last) = idents.pop() else {
            return self.record(Some("?"), name.to_string());
        };
        match idents.as_slice() {
            [] => self.record(None, last),
            [schema] => self.record(Some(schema), last),
            _ => self.record(Some(&idents.join(".")), last),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_view_use_stays_fast_on_nested_and_many_ctes() {
        use std::time::Instant;

        // Each level wraps the last in a non-recursive CTE.
        let mut sql = "SELECT * FROM happyview_records".to_string();
        for i in 0..40 {
            sql = format!("WITH c{i} AS ({sql}) SELECT * FROM c{i}");
        }
        let started = Instant::now();
        assert_eq!(record_view_use(&sql), Ok(RecordViews::Only));
        assert!(started.elapsed().as_secs() < 1, "nested CTEs are slow");

        // The same, with a local table at the bottom.
        let mut sql = "SELECT * FROM my_table, happyview_records".to_string();
        for i in 0..40 {
            sql = format!("WITH c{i} AS ({sql}) SELECT * FROM c{i}");
        }
        assert_eq!(
            record_view_use(&sql),
            Ok(RecordViews::Mixed("my_table".into()))
        );

        // Many sibling CTEs, each reading the one before.
        let mut sql = "WITH c0 AS (SELECT * FROM happyview_records)".to_string();
        for i in 1..1000 {
            sql.push_str(&format!(", c{i} AS (SELECT * FROM c{})", i - 1));
        }
        sql.push_str(" SELECT * FROM c999");
        let started = Instant::now();
        assert_eq!(record_view_use(&sql), Ok(RecordViews::Only));
        assert!(started.elapsed().as_secs() < 1, "many CTEs are slow");
    }

    #[test]
    fn record_view_use_scopes_cte_names() {
        use RecordViews::*;
        let cases: &[(&str, &str)] = &[
            // A CTE inside a join subquery does not hide the outer table.
            (
                "SELECT * FROM happyview_records JOIN (WITH my_table AS (SELECT 1 AS uri) SELECT * FROM my_table) s ON true JOIN my_table m ON m.uri = happyview_records.uri",
                "mixed:my_table",
            ),
            // Nor does one in an IN subquery hide the name in a sibling.
            (
                "SELECT * FROM happyview_records WHERE uri IN (WITH notes AS (SELECT 'x' AS uri) SELECT uri FROM notes) AND did IN (SELECT did FROM notes)",
                "mixed:notes",
            ),
            // A non-recursive CTE does not see itself.
            (
                "WITH my_table AS (SELECT * FROM my_table) SELECT * FROM my_table, happyview_records",
                "mixed:my_table",
            ),
            // A CTE that shadows a view reads whatever its body reads.
            (
                "WITH happyview_records AS (SELECT * FROM my_table) SELECT * FROM happyview_records",
                "mixed:my_table",
            ),
            (
                "WITH recursive t AS (SELECT uri FROM happyview_records UNION ALL SELECT uri FROM t) SELECT * FROM t",
                "only",
            ),
            (
                "WITH a AS (SELECT * FROM happyview_records), b AS (SELECT * FROM a) SELECT * FROM b",
                "only",
            ),
        ];
        for (sql, want) in cases {
            let got = match record_view_use(sql).unwrap() {
                None => "none".to_string(),
                Only => "only".to_string(),
                Mixed(t) => format!("mixed:{t}"),
            };
            assert_eq!(&got, want, "{sql}");
        }
    }

    #[test]
    fn record_view_use_fails_closed_on_hard_shapes() {
        use RecordViews::*;
        let cases: &[(&str, &str)] = &[
            (
                "SELECT uri FROM happyview_records UNION TABLE my_table",
                "mixed:my_table",
            ),
            (
                "SELECT uri FROM happyview_records UNION SELECT uri FROM my_table",
                "mixed:my_table",
            ),
            ("SELECT 1 UNION TABLE happyview_records", "only"),
            (
                "SELECT uri FROM happyview_records UNION TABLE other.my_table",
                "mixed:other.my_table",
            ),
            (
                "SELECT * FROM other.happyview_records",
                "mixed:other.happyview_records",
            ),
            ("SELECT * FROM main.happyview_records", "only"),
            (
                "SELECT * FROM temp.happyview_records",
                "mixed:temp.happyview_records",
            ),
            (
                "SELECT * FROM a.public.happyview_records",
                "mixed:a.public.happyview_records",
            ),
            (
                "SELECT * FROM happyview_records WHERE EXISTS (SELECT 1 FROM my_table)",
                "mixed:my_table",
            ),
            (
                "SELECT * FROM happyview_records WHERE EXISTS (SELECT 1 FROM happyview_record_refs)",
                "only",
            ),
            (
                "SELECT * FROM happyview_records; SELECT * FROM my_table",
                "mixed:my_table",
            ),
            (
                "SELECT * FROM happyview_records; SELECT * FROM happyview_record_refs",
                "only",
            ),
            ("SELECT * FROM \"happyview_records\"", "only"),
            ("SELECT * FROM HappyView_Records", "only"),
            (
                "SELECT * FROM \"HappyView_Records\"",
                "mixed:HappyView_Records",
            ),
            (
                "SELECT * FROM happyview_records, json_each((SELECT data FROM my_table))",
                "mixed:my_table",
            ),
            // A schema-qualified name is never a CTE.
            (
                "WITH my_table AS (SELECT 1) SELECT * FROM happyview_records, public.my_table",
                "mixed:public.my_table",
            ),
        ];
        for (sql, want) in cases {
            let got = match record_view_use(sql).unwrap() {
                None => "none".to_string(),
                Only => "only".to_string(),
                Mixed(t) => format!("mixed:{t}"),
            };
            assert_eq!(&got, want, "{sql}");
        }
        // SQLite-only syntax is outside the generic dialect: refused, not guessed at.
        assert!(
            record_view_use("SELECT * FROM happyview_records INDEXED BY idx WHERE uri = 'a'")
                .is_err()
        );
    }

    #[test]
    fn read_only_rejects_select_into_and_locking_clauses() {
        assert_eq!(
            is_read_only("SELECT * INTO my_table FROM happyview_records"),
            Ok(false)
        );
        assert_eq!(is_read_only("SELECT * FROM t FOR UPDATE"), Ok(false));
        assert_eq!(is_read_only("SELECT * FROM t FOR SHARE"), Ok(false));
        assert_eq!(
            is_read_only("SELECT * FROM (SELECT * FROM t FOR UPDATE) s"),
            Ok(false)
        );
        assert_eq!(is_read_only("SELECT * FROM t"), Ok(true));
    }

    #[test]
    fn the_by_collection_view_is_a_record_view_raw_sql_may_read() {
        let sql = "SELECT uri, record FROM happyview_records_by_collection \
                   WHERE collection_id = hv_collection_id('app.test.post') \
                   ORDER BY created_at_us DESC, id DESC LIMIT 50";
        assert_eq!(check_raw_sql_tables(sql), Ok(()));
        assert_eq!(record_view_use(sql), Ok(RecordViews::Only));
        // The function reads no relation of the query's own.
        assert_eq!(
            record_view_use(
                "SELECT uri FROM happyview_records_by_collection r \
                 JOIN happyview_jobs j ON j.id = r.uri"
            ),
            Ok(RecordViews::Mixed("happyview_jobs".into()))
        );
        // happyview_collections stays refused: hv_collection_id is the way in.
        assert!(
            check_raw_sql_tables(
                "SELECT id FROM happyview_collections WHERE nsid = 'app.test.post'"
            )
            .is_err()
        );
    }

    #[test]
    fn record_view_use_classifies_relations() {
        use RecordViews::*;
        let cases: &[(&str, &str)] = &[
            ("SELECT 1", "none"),
            ("SELECT * FROM my_table", "none"),
            (
                "SELECT * FROM happyview_records WHERE collection = 'a.b.c'",
                "only",
            ),
            (
                "SELECT r.uri FROM happyview_records r JOIN happyview_record_refs f ON f.source_uri = r.uri",
                "only",
            ),
            ("SELECT * FROM public.happyview_records", "only"),
            (
                "WITH x AS (SELECT * FROM happyview_records) SELECT * FROM x",
                "only",
            ),
            (
                "SELECT * FROM happyview_records r, json_each(r.record)",
                "only",
            ),
            (
                "SELECT * FROM happyview_records r JOIN my_table m ON m.uri = r.uri",
                "mixed:my_table",
            ),
            (
                "SELECT * FROM happyview_records WHERE uri IN (SELECT uri FROM happyview_labels)",
                "mixed:happyview_labels",
            ),
            (
                "SELECT (SELECT COUNT(*) FROM happyview_records), (SELECT COUNT(*) FROM notes)",
                "mixed:notes",
            ),
        ];
        for (sql, want) in cases {
            let got = match record_view_use(sql).unwrap() {
                None => "none".to_string(),
                Only => "only".to_string(),
                Mixed(t) => format!("mixed:{t}"),
            };
            assert_eq!(&got, want, "{sql}");
        }
    }

    #[test]
    fn record_view_use_fails_closed_on_unparseable_view_queries() {
        assert!(record_view_use("SELECT * FROM happyview_records WHERE (").is_err());
        // no view token: never parsed, never an error
        assert!(matches!(
            record_view_use("NOT SQL AT ALL"),
            Ok(RecordViews::None)
        ));
    }

    #[test]
    fn raw_sql_allows_non_protected_tables() {
        // Admins can get wild with their own tables.
        assert!(super::check_raw_sql_tables("SELECT * FROM my_table").is_ok());
        assert!(super::check_raw_sql_tables("CREATE TABLE analytics (id INT)").is_ok());
        assert!(super::check_raw_sql_tables("INSERT INTO analytics VALUES (1)").is_ok());
        assert!(super::check_raw_sql_tables("UPDATE analytics SET id = 2").is_ok());
        assert!(super::check_raw_sql_tables("DELETE FROM analytics WHERE id = 1").is_ok());
        assert!(super::check_raw_sql_tables("DROP TABLE analytics").is_ok());
        // A table that merely *contains* the prefix mid-name is fine.
        assert!(super::check_raw_sql_tables("SELECT * FROM myhappyview_data").is_ok());
        // The prefix appearing inside a string literal is not a table reference.
        assert!(
            super::check_raw_sql_tables("INSERT INTO logs (msg) VALUES ('happyview_started')")
                .is_ok()
        );
    }

    #[test]
    fn raw_sql_allows_allowlisted_internal_tables() {
        // Public AppView data and background jobs are readable/writable.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_records").is_ok());
        assert!(
            super::check_raw_sql_tables("DELETE FROM happyview_records WHERE uri = $1").is_ok()
        );
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_record_refs").is_ok());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_labels").is_ok());
        assert!(
            super::check_raw_sql_tables(&format!(
                "SELECT * FROM {}",
                crate::catalog::lexicon_rows::TABLE
            ))
            .is_ok()
        );
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_jobs").is_ok());
    }

    #[test]
    fn raw_sql_blocks_protected_tables() {
        // Secrets / tokens / keys.
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_dpop_keys").is_err());
        assert!(super::check_raw_sql_tables("DROP TABLE happyview_api_keys").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_script_variables").is_err());
        // Auth / privilege / trust config.
        assert!(super::check_raw_sql_tables("UPDATE happyview_users SET is_super = true").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_domains").is_err());
        assert!(super::check_raw_sql_tables("SELECT * FROM happyview_instance_settings").is_err());
        // Space tables, which `happyview.spaces` reaches with access checks.
        for table in [
            "happyview_spaces",
            "happyview_space_members",
            "happyview_space_records",
            "happyview_space_record_oplog",
            "happyview_space_notify_registrations",
            "happyview_space_dids",
            "happyview_space_credentials",
            "happyview_space_repo_state",
        ] {
            assert!(
                super::check_raw_sql_tables(&format!("SELECT * FROM {table}")).is_err(),
                "{table} should be blocked"
            );
        }
        assert!(
            super::check_raw_sql_tables(
                "INSERT INTO happyview_space_members (id, space_id, member_did) VALUES ($1, $2, $3)"
            )
            .is_err()
        );
        // The migration bookkeeping table is off-limits too.
        assert!(super::check_raw_sql_tables("SELECT * FROM _sqlx_migrations").is_err());
    }

    /// Every space table the migrations create, including any added later, is
    /// out of raw SQL's reach.
    #[tokio::test]
    async fn no_migrated_space_table_is_reachable() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let tables: Vec<(String,)> = crate::db::query_as(
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name LIKE 'happyview\\_space%' ESCAPE '\\'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(tables.len() >= 6, "found {tables:?}");
        for (table,) in tables {
            assert!(
                super::check_raw_sql_tables(&format!("SELECT * FROM {table}")).is_err(),
                "{table} is reachable from raw SQL"
            );
        }
    }

    #[test]
    fn raw_sql_blocks_protected_tables_evasion() {
        // Case-insensitive.
        assert!(super::check_raw_sql_tables("SELECT * FROM HAPPYVIEW_USERS").is_err());
        // Double-quoted identifier.
        assert!(super::check_raw_sql_tables(r#"SELECT * FROM "happyview_api_keys""#).is_err());
        // Schema-qualified.
        assert!(
            super::check_raw_sql_tables("SELECT * FROM public.happyview_dpop_sessions").is_err()
        );
        // Second statement in a batch.
        assert!(super::check_raw_sql_tables("SELECT 1; SELECT * FROM happyview_users").is_err());
        // JOIN / subquery position.
        assert!(
            super::check_raw_sql_tables(
                "SELECT * FROM my_table JOIN happyview_api_clients USING (id)"
            )
            .is_err()
        );
        // Unicode-escaped identifier evasion is refused outright.
        assert!(super::check_raw_sql_tables(r#"SELECT * FROM U&"happyview_dpop_keys""#).is_err());
    }

    #[test]
    fn read_only_accepts_selects_and_ctes() {
        assert_eq!(is_read_only("SELECT 1"), Ok(true));
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) SELECT * FROM x"),
            Ok(true)
        );
        assert_eq!(is_read_only("SELECT a FROM t; SELECT b FROM u"), Ok(true));
    }

    #[test]
    fn read_only_rejects_writes_even_after_a_select() {
        assert_eq!(is_read_only("INSERT INTO t VALUES (1)"), Ok(false));
        assert_eq!(is_read_only("SELECT 1; DELETE FROM t"), Ok(false));
        assert_eq!(is_read_only("DROP TABLE t"), Ok(false));
        // Quoted: sqlparser 0.62.0 only accepts a literal value after `PRAGMA
        // name =`, not a bare identifier, so an unquoted `= DELETE` fails to
        // parse at all rather than parsing as a (non-Query) Pragma statement.
        assert_eq!(is_read_only("PRAGMA journal_mode = 'DELETE'"), Ok(false));
    }

    #[test]
    fn read_only_rejects_writes_hidden_in_query_statements() {
        // sqlparser parses these as Statement::Query with a write SetExpr body,
        // not as Statement::Delete/Insert/Update — a naive
        // `matches!(s, Statement::Query(_))` check would wrongly accept them.
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) DELETE FROM happyview_records"),
            Ok(false)
        );
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) INSERT INTO happyview_records VALUES (1)"),
            Ok(false)
        );
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) UPDATE happyview_records SET uri = 'a'"),
            Ok(false)
        );
        // A write hidden inside the CTE body itself, not the outer statement.
        assert_eq!(
            is_read_only("WITH x AS (DELETE FROM happyview_records RETURNING uri) SELECT * FROM x"),
            Ok(false)
        );
    }

    #[test]
    fn is_write_tells_writes_from_reads() {
        for sql in [
            "UPDATE happyview_records SET rev = 'x' WHERE collection = 'a.b.c'",
            "DELETE FROM happyview_record_refs WHERE collection = 'a.b.c'",
            "INSERT INTO my_table SELECT uri FROM happyview_records",
            "WITH x AS (SELECT 1) DELETE FROM happyview_records",
            "CREATE TABLE t AS SELECT * FROM happyview_records",
            "UPDATE t SET a = 1; DELETE FROM u",
        ] {
            assert!(is_write(sql), "{sql}");
        }
        for sql in [
            "SELECT * FROM happyview_records",
            "SELECT * FROM happyview_records FOR SHARE",
            "SELECT * INTO t FROM happyview_records",
            // A data-modifying CTE under a read is not a plain write.
            "WITH x AS (DELETE FROM happyview_records RETURNING uri) SELECT * FROM x",
            // Neither is a batch mixing a read with a write.
            "SELECT * FROM happyview_records; DELETE FROM t",
            "SELECT * FROM happyview_records INDEXED BY no_such_index",
            "",
        ] {
            assert!(!is_write(sql), "{sql}");
        }
    }

    #[test]
    fn record_view_use_refuses_too_many_ctes() {
        let ctes = |n: usize| {
            let mut sql = "WITH c0 AS (SELECT * FROM happyview_records)".to_string();
            for i in 1..n {
                sql.push_str(&format!(", c{i} AS (SELECT * FROM c{})", i - 1));
            }
            sql.push_str(&format!(" SELECT * FROM c{}", n - 1));
            sql
        };
        assert_eq!(record_view_use(&ctes(MAX_CTES)), Ok(RecordViews::Only));
        let err = record_view_use(&ctes(MAX_CTES + 1)).unwrap_err();
        assert!(err.contains("CTEs"), "{err}");
        // Nested inside a subquery too.
        let nested = format!("SELECT * FROM ({}) s", ctes(MAX_CTES + 1));
        assert!(record_view_use(&nested).is_err());
    }

    #[test]
    fn read_only_accepts_ctes_and_set_operations() {
        assert_eq!(
            is_read_only("WITH x AS (SELECT 1) SELECT * FROM x UNION SELECT 2"),
            Ok(true)
        );
        assert_eq!(is_read_only("SELECT 1 UNION ALL SELECT 2"), Ok(true));
    }

    #[test]
    fn claimed_record_writes_are_refused_and_others_pass() {
        let claims = |c: &str| c == "app.claimed.thing";
        let check = |sql: &str, params: &[serde_json::Value]| {
            check_claimed_record_writes(sql, params, &claims)
        };
        // Reads, and writes elsewhere, pass whatever they name.
        assert!(
            check(
                "SELECT * FROM happyview_records WHERE collection = 'app.claimed.thing'",
                &[]
            )
            .is_ok()
        );
        assert!(check("DELETE FROM my_table", &[]).is_ok());
        // A claimed collection, as a literal, a URI or a parameter.
        for (sql, params) in [
            (
                "INSERT INTO happyview_records (collection) VALUES ('app.claimed.thing')",
                vec![],
            ),
            (
                "DELETE FROM happyview_records WHERE uri = 'at://did:plc:a/app.claimed.thing/1'",
                vec![],
            ),
            (
                "UPDATE happyview_records SET record = ? WHERE collection = 'app.local.thing' OR collection = ?",
                vec![
                    serde_json::json!("{}"),
                    serde_json::json!("app.claimed.thing"),
                ],
            ),
        ] {
            let err = check(sql, &params).unwrap_err();
            assert!(err.starts_with(CLAIMED_WRITE_REFUSED), "{err}");
            assert!(err.ends_with("app.claimed.thing"), "{err}");
        }
        // A local collection, by literal or parameter.
        assert!(
            check(
                "DELETE FROM happyview_records WHERE collection = 'app.local.thing'",
                &[]
            )
            .is_ok()
        );
        assert!(
            check(
                "DELETE FROM HAPPYVIEW_RECORDS WHERE uri = ?",
                &[serde_json::json!("at://did:plc:a/app.local.thing/1")]
            )
            .is_ok()
        );
        // Nothing to check against: refused.
        assert!(check("DELETE FROM happyview_records WHERE did = 'did:plc:a'", &[]).is_err());
        assert!(check("DELETE FROM happyview_records", &[]).is_err());
    }

    #[test]
    fn read_only_fails_closed_on_unparseable_sql() {
        // "SELECT FROM WHERE" is not actually a parse error under sqlparser's
        // generic dialect ("WHERE" reads as a table name), so this uses SQL
        // that genuinely cannot be parsed as any statement.
        assert!(is_read_only("SELECT 1 FROM (").is_err());
    }
}

#[cfg(test)]
mod postgres_plugin_guard_tests {
    //! Parse-level checks for the Postgres plugin-SQL allowlist. No database:
    //! these assert what the parser accepts and refuses. The behaviour that the
    //! least-privileged role denies the same statements at the database is
    //! covered by the integration tests in `tests/`.
    use super::{PluginSqlMode::*, check_postgres_plugin_sql};

    // The second boolean is `roles_available`. Most checks hold either way; the
    // function allowlist applies only when the roles are *not* available.
    const EITHER: [bool; 2] = [true, false];

    #[test]
    fn allows_plain_reads_of_allowlisted_relations() {
        for roles in EITHER {
            for sql in [
                "SELECT * FROM happyview_records WHERE collection = $1",
                "SELECT count(*) FROM happyview_records_by_collection",
                "SELECT lower(uri) FROM happyview_record_refs LIMIT 10",
                "SELECT * FROM public.happyview_labels",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Read, roles).is_ok(),
                    "should allow (roles={roles}): {sql}"
                );
            }
        }
    }

    #[test]
    fn allows_caller_owned_tables_and_their_ddl_under_the_roles() {
        // Plugins are expected to use their own tables through raw SQL. DDL is
        // allowed when the roles are in force; DML and reads either way.
        for sql in [
            "CREATE TABLE IF NOT EXISTS my_plugin_state (k TEXT, v TEXT)",
            "INSERT INTO my_plugin_state (k, v) VALUES ($1, $2)",
            "CREATE INDEX my_plugin_state_k ON my_plugin_state (k)",
            "ALTER TABLE my_plugin_state ADD COLUMN n INT",
            "CREATE VIEW my_plugin_view AS SELECT k FROM my_plugin_state",
            "DROP TABLE my_plugin_state",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, true).is_ok(),
                "should allow a caller-owned table under the roles: {sql}"
            );
        }
        for roles in EITHER {
            assert!(
                check_postgres_plugin_sql("INSERT INTO my_plugin_state VALUES ($1)", Write, roles)
                    .is_ok()
            );
            assert!(
                check_postgres_plugin_sql("SELECT * FROM my_plugin_state", Read, roles).is_ok()
            );
        }
    }

    #[test]
    fn refuses_ddl_in_the_fallback() {
        // Without the roles the parser is the only boundary and cannot vouch for
        // a function hidden in a column DEFAULT/CHECK, so DDL is refused.
        for sql in [
            "CREATE TABLE my_plugin_state (k TEXT)",
            "CREATE INDEX i ON my_plugin_state (k)",
            "CREATE VIEW v AS SELECT 1",
            "ALTER TABLE my_plugin_state ADD COLUMN n INT",
            "DROP TABLE my_plugin_state",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, false).is_err(),
                "fallback should refuse DDL: {sql}"
            );
        }
    }

    #[test]
    fn refuses_creating_executable_objects_in_either_mode() {
        // The CVE-2018-1058 object-shadowing surface: nothing a plugin creates
        // may define code HappyView's own queries could resolve to.
        for roles in EITHER {
            for sql in [
                "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql",
                "CREATE OR REPLACE FUNCTION public.lower(varchar) RETURNS text AS $$ SELECT 'x' $$ LANGUAGE sql",
                "CREATE PROCEDURE p() AS $$ BEGIN END $$ LANGUAGE plpgsql",
                "CREATE OPERATOR === (leftarg = int, rightarg = int, function = int4eq)",
                "CREATE CAST (int AS text) WITH FUNCTION f(int)",
                "CREATE AGGREGATE agg(int) (sfunc = int4pl, stype = int)",
                "CREATE TRIGGER t BEFORE INSERT ON my_plugin_state FOR EACH ROW EXECUTE FUNCTION f()",
                "CREATE RULE r AS ON INSERT TO my_plugin_state DO INSTEAD NOTHING",
                "CREATE EXTENSION dblink",
                "CREATE LANGUAGE plpython3u",
                "CREATE EVENT TRIGGER et ON ddl_command_start EXECUTE FUNCTION f()",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, roles).is_err(),
                    "should refuse creating executable objects (roles={roles}): {sql}"
                );
            }
        }
    }

    #[test]
    fn refuses_a_function_overload_matching_a_builtin() {
        // Specifically the shadowing shape: define `public.lower(varchar)` so a
        // later `lower(x)` in HappyView's own query would resolve to it.
        for roles in EITHER {
            assert!(
                check_postgres_plugin_sql(
                    "CREATE FUNCTION public.lower(varchar) RETURNS text AS $$ SELECT 'x'::text $$ LANGUAGE sql",
                    Write,
                    roles
                )
                .is_err(),
                "should refuse a function overload (roles={roles})"
            );
        }
    }

    #[test]
    fn refuses_set_config_in_either_mode() {
        for roles in EITHER {
            for sql in [
                "SELECT set_config('search_path', 'evil', false)",
                "SELECT pg_catalog.set_config('role', 'postgres', false)",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Read, roles).is_err(),
                    "should refuse set_config (roles={roles}): {sql}"
                );
            }
        }
    }

    #[test]
    fn refuses_alter_table_owner_in_either_mode() {
        for roles in EITHER {
            assert!(
                check_postgres_plugin_sql(
                    "ALTER TABLE my_plugin_state OWNER TO postgres",
                    Write,
                    roles
                )
                .is_err(),
                "should refuse ALTER TABLE OWNER (roles={roles})"
            );
        }
    }

    #[test]
    fn allows_writes_only_in_write_mode() {
        let sql = "INSERT INTO happyview_jobs (id) VALUES ($1)";
        assert!(check_postgres_plugin_sql(sql, Write, true).is_ok());
        assert!(check_postgres_plugin_sql(sql, Read, true).is_err());
    }

    #[test]
    fn string_executing_builtins_are_refused_only_in_the_fallback() {
        // The reads that the tokenizer guard let through: the table name lives
        // in a string the built-in would execute or resolve. With the roles
        // unavailable the parser is the boundary and the function allowlist
        // refuses them. With the roles available the parser lets them through —
        // the role denies the protected read at the database (see the
        // integration tests), so a plugin keeps full use of functions.
        for sql in [
            "SELECT query_to_xml('select 1', true, false, '')",
            "SELECT table_to_xml('x'::regclass, true, false, '')",
            "SELECT query_to_xml_and_xmlschema('select 1', true, false, '')",
            "SELECT pg_read_file('/etc/passwd')",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Read, false).is_err(),
                "fallback should refuse: {sql}"
            );
            assert!(
                check_postgres_plugin_sql(sql, Read, true).is_ok(),
                "with roles the parser allows (role denies at the db): {sql}"
            );
        }
    }

    #[test]
    fn refuses_internal_tables_in_either_mode_whether_or_not_roles_are_set_up() {
        for roles in EITHER {
            for sql in [
                "SELECT * FROM happyview_users",
                "SELECT token_hash FROM happyview_api_keys",
                "SELECT * FROM happyview_oauth_sessions",
                "SELECT * FROM public.happyview_users",
                "SELECT * FROM \"happyview_users\"",
                // Hidden inside a data-modifying CTE.
                "WITH x AS (DELETE FROM happyview_users RETURNING id) SELECT * FROM x",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, roles).is_err(),
                    "should refuse the internal table (roles={roles}): {sql}"
                );
            }
        }
    }

    #[test]
    fn refuses_escape_statements_in_either_mode() {
        for roles in EITHER {
            for sql in [
                "DO $$ BEGIN PERFORM 1; END $$",
                "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql",
                "COPY my_table TO '/tmp/x'",
                "SET ROLE postgres",
                "RESET ROLE",
                "SELECT 1; RESET ROLE",
                "BEGIN",
                "GRANT ALL ON my_table TO postgres",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, roles).is_err(),
                    "should refuse the escape statement (roles={roles}): {sql}"
                );
            }
        }
    }

    #[test]
    fn fallback_function_check_sees_every_dml_position() {
        // The completeness claim: in the DML-only fallback a string-executing
        // function is caught wherever it hides — a CTE, a scalar subquery, a
        // RETURNING list, an ON CONFLICT update, a LATERAL subquery. Each must
        // be refused with the roles unavailable.
        for sql in [
            "WITH x AS (SELECT query_to_xml('', true, false, '') AS q) SELECT * FROM x",
            "SELECT (SELECT query_to_xml('', true, false, '')) AS q",
            "INSERT INTO caller_t (k) VALUES ('a') RETURNING query_to_xml('', true, false, '')",
            "INSERT INTO caller_t (k) VALUES ('a') ON CONFLICT (k) DO UPDATE SET v = query_to_xml('', true, false, '')",
            "SELECT * FROM caller_t, LATERAL (SELECT query_to_xml('', true, false, '')) s",
            "UPDATE caller_t SET v = query_to_xml('', true, false, '')",
            "DELETE FROM caller_t WHERE k = query_to_xml('', true, false, '')",
        ] {
            assert!(
                check_postgres_plugin_sql(sql, Write, false).is_err(),
                "fallback must refuse a hidden string-executing function: {sql}"
            );
        }
    }

    #[test]
    fn internal_table_is_seen_in_every_position() {
        // The same completeness, for the relation deny — it holds in either mode.
        for roles in EITHER {
            for sql in [
                "WITH x AS (SELECT id FROM happyview_users) SELECT * FROM x",
                "SELECT (SELECT count(*) FROM happyview_api_keys) AS n",
                "SELECT * FROM caller_t WHERE k IN (SELECT token_hash FROM happyview_api_keys)",
                "SELECT * FROM caller_t, LATERAL (SELECT id FROM happyview_users) u",
            ] {
                assert!(
                    check_postgres_plugin_sql(sql, Write, roles).is_err(),
                    "must refuse the internal table hidden in a subposition (roles={roles}): {sql}"
                );
            }
        }
    }

    #[test]
    fn refuses_unparseable_sql() {
        assert!(check_postgres_plugin_sql("SELECT 1 FROM (", Read, true).is_err());
        assert!(check_postgres_plugin_sql("", Read, true).is_err());
    }
}
